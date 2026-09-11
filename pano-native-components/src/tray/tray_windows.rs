use crate::JniCallback;
use crate::tray::{OUTGOING_TRAY_EVENT_TX, PanoTrayData, TRAY_DATA_TX, TRAY_NAME, send_tray_click};
use std::cell::{Cell, OnceCell, RefCell};
use std::mem::size_of;
use std::time::Duration;
use tokio::sync::mpsc;
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::LoadIconWithScaleDown;
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIM_ADD, NIM_MODIFY, NIM_SETVERSION, NIN_SELECT,
    NOTIFYICON_VERSION_4, NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyIcon, DestroyMenu,
    DispatchMessageW, GetCursorPos, GetSystemMetrics, HICON, HMENU, KillTimer, MF_GRAYED,
    MF_SEPARATOR, MF_STRING, MSG, PM_REMOVE, PeekMessageW, PostMessageW, RegisterClassW,
    RegisterWindowMessageW, SM_CXSMICON, SetForegroundWindow, SetTimer, TPM_RIGHTBUTTON,
    TrackPopupMenu, TranslateMessage, WINDOW_STYLE, WM_APP, WM_CLOSE, WM_COMMAND, WM_DPICHANGED,
    WM_LBUTTONDBLCLK, WM_NULL, WM_QUERYENDSESSION, WM_RBUTTONUP, WM_SETTINGCHANGE, WM_TIMER,
    WNDCLASSW, WS_EX_TOPMOST,
};
use windows::core::{HSTRING, PCWSTR, w};
use windows_registry::CURRENT_USER;

const WM_TRAYICON: u32 = WM_APP + 1;
const TRAY_UID: u32 = 1;
const CMD_ID_BASE: u32 = 1000;
const TIMER_ID_CLICK: usize = 1;
const DBL_CLICK_THRESHOLD_MS: u32 = 100;

thread_local! {
    static TRAY_DATA: RefCell<Option<PanoTrayData>> = const { RefCell::new(None) };
    static WM_TASKBAR_CREATED: RefCell<u32> = const { RefCell::new(0) };
    static SUPPRESS_NEXT_LBUTTONUP: RefCell<bool> = const { RefCell::new(false) };
    static RECOMPUTE_READD_TRIGGER: OnceCell<mpsc::Sender<bool>> = const { OnceCell::new() };
    static TASKBAR_LIGHT_CACHE: Cell<bool> = const { Cell::new(true) };
}

// true readds it
fn send_recompute_readd_event(event: bool) {
    RECOMPUTE_READD_TRIGGER.with(|d| {
        if let Some(sender) = d.get() {
            let _ = sender.try_send(event);
        }
    });
}

fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let taskbar_created_id = WM_TASKBAR_CREATED.with(|m| *m.borrow());

    match msg {
        msg if msg == taskbar_created_id => {
            send_recompute_readd_event(true);
            LRESULT(0)
        }

        WM_TRAYICON => {
            let event = (lparam.0 as u32) & 0xFFFF;
            match event {
                NIN_SELECT => {
                    let suppress = SUPPRESS_NEXT_LBUTTONUP.with(|s| {
                        let v = *s.borrow();
                        *s.borrow_mut() = false;
                        v
                    });
                    if !suppress {
                        let _ = unsafe {
                            SetTimer(Some(hwnd), TIMER_ID_CLICK, DBL_CLICK_THRESHOLD_MS, None)
                        };
                    }
                }
                WM_LBUTTONDBLCLK => {
                    let _ = unsafe { KillTimer(Some(hwnd), TIMER_ID_CLICK) };
                    SUPPRESS_NEXT_LBUTTONUP.with(|s| *s.borrow_mut() = true);
                    send_tray_click("Open");
                }
                WM_RBUTTONUP => {
                    let _ = unsafe { KillTimer(Some(hwnd), TIMER_ID_CLICK) };
                    show_menu(hwnd)
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_TIMER => {
            if wparam.0 == TIMER_ID_CLICK {
                let _ = unsafe { KillTimer(Some(hwnd), TIMER_ID_CLICK) };
                show_menu(hwnd);
            }
            LRESULT(0)
        }
        WM_COMMAND => {
            let cmd = (wparam.0 & 0xFFFF) as u32;
            if cmd >= CMD_ID_BASE {
                let idx = (cmd - CMD_ID_BASE) as usize;
                let clicked_id = TRAY_DATA.with(|d| {
                    d.borrow()
                        .as_ref()
                        .and_then(|d| d.menu_items.get(idx))
                        .map(|(id, _)| id.clone())
                });
                if let Some(id) = clicked_id {
                    send_tray_click(id);
                }
            }
            LRESULT(0)
        }

        // WM_QUERYENDSESSION: on user logoff/shutdown
        // WM_CLOSE: when the compose block hasn't run yet.
        WM_QUERYENDSESSION | WM_CLOSE => {
            send_tray_click("Exit");
            if msg == WM_QUERYENDSESSION {
                LRESULT(1) // tell windows we're fine with shutdown proceeding
            } else {
                LRESULT(0)
            }
        }

        WM_SETTINGCHANGE => {
            if setting_change_is_immersive_color(lparam) {
                update_app_theme();

                if update_taskbar_theme_cache() {
                    send_recompute_readd_event(false);
                }
            }
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }

        WM_DPICHANGED => {
            send_recompute_readd_event(false);
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }

        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

fn build_menu() -> windows::core::Result<HMENU> {
    let hmenu = unsafe { CreatePopupMenu()? };
    TRAY_DATA.with(|d| {
        let data = d.borrow();
        let items = data
            .as_ref()
            .map(|d| d.menu_items.as_slice())
            .unwrap_or(&[]);
        for (idx, (id, text)) in items.iter().enumerate() {
            if id == "Separator" {
                unsafe { AppendMenuW(hmenu, MF_SEPARATOR, 0, PCWSTR::null())? };
            } else {
                let mut flags = MF_STRING;
                if id.ends_with("Disabled") {
                    flags |= MF_GRAYED;
                }
                let wide = to_wide(&text.replace('&', "&&"));
                unsafe {
                    AppendMenuW(
                        hmenu,
                        flags,
                        CMD_ID_BASE as usize + idx,
                        PCWSTR(wide.as_ptr()),
                    )?
                };
            }
        }
        Ok::<(), windows::core::Error>(())
    })?;
    Ok(hmenu)
}

fn show_menu(hwnd: HWND) {
    match build_menu() {
        Ok(hmenu) => unsafe {
            let mut pt = POINT::default();
            let _ = GetCursorPos(&mut pt);
            // Required so the menu dismisses correctly when the user clicks away.
            let _ = SetForegroundWindow(hwnd);
            let _ = TrackPopupMenu(hmenu, TPM_RIGHTBUTTON, pt.x, pt.y, None, hwnd, None);
            let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
            let _ = DestroyMenu(hmenu);
        },
        Err(e) => log::error!("Failed to build tray menu: {e}"),
    }
}

fn update_taskbar_theme_cache() -> bool {
    let new_value =     // Taskbar follows "SystemUsesLightTheme", not "AppsUseLightTheme"
    CURRENT_USER
        .open(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize")
        .and_then(|k| k.get_u32("SystemUsesLightTheme"))
        .map(|v| v != 0)
        .unwrap_or(false);

    TASKBAR_LIGHT_CACHE.with(|c| {
        let changed = c.get() != new_value;
        c.set(new_value);
        changed
    })
}

fn update_app_theme() {
    let is_light = CURRENT_USER
        .open(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize")
        .and_then(|k| k.get_u32("AppsUseLightTheme"))
        .map(|v| v != 0)
        .unwrap_or(false);

    if let Some(tx) = OUTGOING_TRAY_EVENT_TX.get() {
        let _ = tx.try_send(JniCallback::DarkModeChanged(!is_light));
    }
}

// WM_SETTINGCHANGE fires for many unrelated settings
// only the "ImmersiveColorSet" payload corresponds to a light/dark theme change.
fn setting_change_is_immersive_color(lparam: LPARAM) -> bool {
    if lparam.0 == 0 {
        return false;
    }
    let pcwstr = PCWSTR(lparam.0 as *const u16);
    unsafe { pcwstr.to_string() }
        .map(|s| s == "ImmersiveColorSet")
        .unwrap_or(false)
}

fn compute_hicon(dir: &str, icon_name: &str) -> windows::core::Result<HICON> {
    let dark = !TASKBAR_LIGHT_CACHE.with(|c| c.get());
    let size = unsafe { GetSystemMetrics(SM_CXSMICON) };

    if dir.is_empty() {
        let base_id = if icon_name.contains("idle") {
            100
        } else if icon_name.contains("scrobbling") {
            110
        } else if icon_name.contains("error") {
            120
        } else {
            100
        };
        let id = if dark { base_id + 1 } else { base_id };

        let hinstance: HINSTANCE = unsafe { GetModuleHandleW(None) }?.into();

        unsafe {
            LoadIconWithScaleDown(
                hinstance.into(),
                PCWSTR::from_raw(id as *const _),
                size,
                size,
            )
        }
    } else {
        let theme = if dark { "white" } else { "black" };
        let path = std::path::Path::new(dir).join(format!("{}-{}.ico", icon_name, theme));
        let wpath = HSTRING::from(path.to_string_lossy().as_ref());

        unsafe { LoadIconWithScaleDown(None, PCWSTR::from_raw(wpath.as_ptr()), size, size) }
    }
}

struct WindowsTray {
    hwnd: HWND,
    hicon: Option<HICON>,
    icon_name: Option<String>,
}

impl WindowsTray {
    fn new(data: PanoTrayData) -> windows::core::Result<Self> {
        unsafe {
            let hinstance = GetModuleHandleW(None)?;
            let class_name_hstring = HSTRING::from(TRAY_NAME);
            let class_name = PCWSTR(class_name_hstring.as_ptr());

            let wc = WNDCLASSW {
                lpfnWndProc: Some(wndproc),
                hInstance: hinstance.into(),
                lpszClassName: class_name,
                ..Default::default()
            };
            // Ignore failure: benign if the process already registered it once.
            RegisterClassW(&wc);

            let msg_id = RegisterWindowMessageW(w!("TaskbarCreated"));
            WM_TASKBAR_CREATED.with(|m| *m.borrow_mut() = msg_id);

            let hwnd = CreateWindowExW(
                // WINDOW_EX_STYLE::default(),
                WS_EX_TOPMOST,
                class_name,
                w!("PanoTray"),
                WINDOW_STYLE::default(),
                0,
                0,
                0,
                0,
                None,
                None,
                Some(hinstance.into()),
                None,
            )?;

            // init the cache
            update_taskbar_theme_cache();

            let mut tray = Self {
                hwnd,
                hicon: None,
                icon_name: None,
            };
            tray.apply(data)?;
            Ok(tray)
        }
    }

    fn refresh_icon(&mut self, re_add: bool) -> windows::core::Result<()> {
        TRAY_DATA.with(|d| {
            let data_ref = d.borrow();
            let Some(data) = data_ref.as_ref() else {
                return Ok(());
            };

            let new_icon = compute_hicon(&data.icons_dir, &data.icon_name)?;
            self.set_icon(new_icon, &data.tooltip, re_add)?;
            self.swap_icon(new_icon, data.icons_dir.is_empty());

            Ok(())
        })
    }

    // Stores new_icon, destroying the previous one if not managed by Windows.
    fn swap_icon(&mut self, new_icon: HICON, owned: bool) {
        if let Some(old_icon) = self.hicon.replace(new_icon)
            && owned
        {
            unsafe {
                let _ = DestroyIcon(old_icon);
            }
        }
    }

    fn set_icon(&self, hicon: HICON, tooltip: &str, add: bool) -> windows::core::Result<()> {
        unsafe {
            let mut nid = NOTIFYICONDATAW {
                cbSize: size_of::<NOTIFYICONDATAW>() as u32,
                hWnd: self.hwnd,
                uID: TRAY_UID,
                uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP,
                uCallbackMessage: WM_TRAYICON,
                hIcon: hicon,
                ..Default::default()
            };

            let wide_tooltip = to_wide(tooltip);
            let wide_tooltip_len = wide_tooltip.len().min(127);
            nid.szTip[..wide_tooltip_len].copy_from_slice(&wide_tooltip[..wide_tooltip_len]);

            if add {
                let _ = Shell_NotifyIconW(NIM_ADD, &nid);
                nid.Anonymous.uVersion = NOTIFYICON_VERSION_4;
                let _ = Shell_NotifyIconW(NIM_SETVERSION, &nid);
            } else {
                let _ = Shell_NotifyIconW(NIM_MODIFY, &nid);
            }
        }

        Ok(())
    }

    fn apply(&mut self, data: PanoTrayData) -> windows::core::Result<()> {
        let icon_changed = self.icon_name.as_ref() != Some(&data.icon_name);
        let add = self.hicon.is_none();

        if icon_changed {
            let new_icon = compute_hicon(&data.icons_dir, &data.icon_name).unwrap_or_default();
            self.set_icon(new_icon, &data.tooltip, add)?;
            self.swap_icon(new_icon, data.icons_dir.is_empty());
        } else {
            self.set_icon(self.hicon.unwrap_or_default(), &data.tooltip, add)?;
        }

        self.icon_name = Some(data.icon_name.clone());
        TRAY_DATA.replace(Some(data));

        Ok(())
    }
}

fn pump_messages() {
    unsafe {
        let mut msg = MSG::default();
        while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

pub async fn tray_listener(
    callback_sender: mpsc::Sender<JniCallback>,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut receiver = TRAY_DATA_TX.subscribe();
    OUTGOING_TRAY_EVENT_TX.set(callback_sender).unwrap();

    update_app_theme();

    let (recompute_readd_tx, mut recompute_readd_rx) = mpsc::channel::<bool>(4);
    RECOMPUTE_READD_TRIGGER.with(|d| {
        d.set(recompute_readd_tx).unwrap();
    });

    let mut tray: Option<WindowsTray> = None;
    let mut pump_interval = tokio::time::interval(Duration::from_millis(50));
    pump_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    // apply whatever tray data was already set before this loop started
    if let Some(tray_data) = receiver.borrow_and_update().clone() {
        match WindowsTray::new(tray_data) {
            Ok(t) => tray = Some(t),
            Err(e) => log::error!("Failed to create tray window: {e}"),
        }
    }

    loop {
        tokio::select! {
            biased;
            _ = pump_interval.tick() => {
                pump_messages();
            }

            Some(re_add) = recompute_readd_rx.recv() => {
                if let Some(ref mut tray) = tray {
                    let result = tray.refresh_icon(re_add);
                    if let Err(e) = result {
                        log::error!("Failed to update tray icon: {e}");
                    }
                }
            }

            changed = receiver.changed() => {
                if changed.is_err() {
                    break;
                }

                if let Some(tray_data) = receiver.borrow_and_update().clone() {
                    match &mut tray {
                        None => match WindowsTray::new(tray_data) {
                            Ok(t) => tray = Some(t),
                            Err(e) => log::error!("Failed to create tray window: {e}"),
                        },
                        Some(t) => {
                            if let Err(e) = t.apply(tray_data) {
                                log::error!("Failed to update tray: {e}");
                            }
                        }
                    }
                }
            }
        }
    }

    Ok(())
}
