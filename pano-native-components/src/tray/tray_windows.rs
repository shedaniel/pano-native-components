use crate::JniCallback;
use std::cell::{OnceCell, RefCell};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::Cursor;
use std::mem::size_of;
use std::time::Duration;
use tokio::sync::mpsc;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIM_ADD, NIM_MODIFY, NIM_SETVERSION, NIN_SELECT,
    NOTIFYICON_VERSION_4, NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreateIconFromResourceEx, CreatePopupMenu, CreateWindowExW, DefWindowProcW,
    DestroyIcon, DestroyMenu, DispatchMessageW, GetCursorPos, HICON, HMENU, KillTimer,
    LR_DEFAULTCOLOR, MF_GRAYED, MF_SEPARATOR, MF_STRING, MSG, PM_REMOVE, PeekMessageW,
    PostMessageW, RegisterClassW, RegisterWindowMessageW, SetForegroundWindow, SetTimer,
    TPM_RIGHTBUTTON, TrackPopupMenu, TranslateMessage, WINDOW_STYLE, WM_APP, WM_COMMAND,
    WM_LBUTTONDBLCLK, WM_NULL, WM_RBUTTONUP, WM_TIMER, WNDCLASSW, WS_EX_TOPMOST,
};
use windows::core::{PCWSTR, w};

const WM_TRAYICON: u32 = WM_APP + 1;
const TRAY_UID: u32 = 1;
const CMD_ID_BASE: u32 = 1000;
const TIMER_ID_CLICK: usize = 1;
const DBL_CLICK_THRESHOLD_MS: u32 = 150;
const ICON_DIMENSION: i32 = 128;

thread_local! {
    static TRAY_DATA: RefCell<Option<super::PanoTrayData>> = const { RefCell::new(None) };
    static WM_TASKBAR_CREATED: RefCell<u32> = const { RefCell::new(0) };
    static SUPPRESS_NEXT_LBUTTONUP: RefCell<bool> = const { RefCell::new(false) };
    static TASKBAR_RECREATED_TRIGGER: OnceCell<mpsc::Sender<()>> = const { OnceCell::new() };
}

fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let taskbar_created_id = WM_TASKBAR_CREATED.with(|m| *m.borrow());

    match msg {
        msg if msg == taskbar_created_id => {
            TASKBAR_RECREATED_TRIGGER.with(|d| {
                if let Some(sender) = d.get() {
                    let _ = sender.try_send(());
                }
            });
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
                    if let Some(tx) = super::OUTGOING_TRAY_EVENT_TX.get() {
                        let _ = tx.try_send(JniCallback::TrayItemClicked("Open".to_string()));
                    }
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
                TRAY_DATA.with(|d| {
                    if let Some(id) = d
                        .borrow()
                        .as_ref()
                        .and_then(|d| d.menu_items.get(idx))
                        .map(|(id, _)| id.clone())
                        && let Some(tx) = super::OUTGOING_TRAY_EVENT_TX.get()
                    {
                        let _ = tx.try_send(JniCallback::TrayItemClicked(id));
                    }
                });
            }
            LRESULT(0)
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
                let wide = to_wide(text);
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

fn compute_hicon(png_bytes: &[u8], invert: bool) -> windows::core::Result<HICON> {
    let mut buf = Cursor::new(Vec::new());
    let png_bytes = if invert {
        let img = image::load_from_memory_with_format(png_bytes, image::ImageFormat::Png);
        if let Ok(mut img) = img {
            img.invert();
            img.write_to(&mut buf, image::ImageFormat::Png).unwrap();
            buf.get_ref()
        } else {
            log::error!("invalid png");
            png_bytes
        }
    } else {
        png_bytes
    };

    unsafe {
        CreateIconFromResourceEx(
            png_bytes,
            true,
            0x00030000,
            ICON_DIMENSION,
            ICON_DIMENSION,
            LR_DEFAULTCOLOR,
        )
    }
}

struct WindowsTray {
    hwnd: HWND,
    hicon: Option<HICON>,
    icon_hash: u64,
}

impl WindowsTray {
    fn new(data: super::PanoTrayData) -> windows::core::Result<Self> {
        unsafe {
            let hinstance = GetModuleHandleW(None)?;
            let class_name = w!("PanoTrayWindowClass");

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

            let mut tray = Self {
                hwnd,
                hicon: None,
                icon_hash: 0,
            };
            tray.apply(data)?;
            Ok(tray)
        }
    }

    fn re_add_icon(&self) -> windows::core::Result<()> {
        TRAY_DATA.with(|d| {
            if let Some(data) = d.borrow().as_ref()
                && let Some(hicon) = self.hicon
            {
                self.set_icon(hicon, &data.tooltip, true)
            } else {
                Ok(())
            }
        })
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

    fn apply(&mut self, data: super::PanoTrayData) -> windows::core::Result<()> {
        let mut hasher = DefaultHasher::new();
        data.png_bytes.hash(&mut hasher);
        data.invert.hash(&mut hasher);
        let icon_hash = hasher.finish();

        let icon = if icon_hash == self.icon_hash {
            self.hicon
        } else {
            compute_hicon(&data.png_bytes, data.invert).ok()
        };

        let add = self.hicon.is_none();

        self.set_icon(icon.unwrap_or_default(), &data.tooltip, add)?;

        if icon_hash != self.icon_hash
            && let Some(old_icon) = self.hicon.replace(icon.unwrap_or_default())
        {
            unsafe {
                let _ = DestroyIcon(old_icon);
            }
        }

        self.icon_hash = icon_hash;
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
    let (sender, mut receiver) = mpsc::channel::<super::PanoTrayData>(10);
    super::TOKIO_USER_EVENT_SENDER.set(sender).unwrap();
    super::OUTGOING_TRAY_EVENT_TX.set(callback_sender).unwrap();

    let (taskbar_recreated_tx, mut taskbar_recreated_rx) = mpsc::channel::<()>(1);
    TASKBAR_RECREATED_TRIGGER.with(|d| {
        d.set(taskbar_recreated_tx).unwrap();
    });

    let mut tray: Option<WindowsTray> = None;
    let mut pump_interval = tokio::time::interval(Duration::from_millis(50));
    pump_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            biased;
            _ = pump_interval.tick() => {
                pump_messages();
            }

            _ = taskbar_recreated_rx.recv() => {
                if let Some(ref tray) = tray {
                    tray.re_add_icon().unwrap_or_else(|e| {
                        log::error!("Failed to re-add tray icon after taskbar recreation: {e}");
                    });
                }
            }

            data = receiver.recv() => {
                match data {
                    Some(tray_data) => match &mut tray {
                        None => match WindowsTray::new(tray_data) {
                            Ok(t) => tray = Some(t),
                            Err(e) => log::error!("Failed to create tray window: {e}"),
                        },
                        Some(t) => {
                            if let Err(e) = t.apply(tray_data) {
                                log::error!("Failed to update tray: {e}");
                            }
                        }
                    },
                    None => break,
                }
            }
        }
    }

    Ok(())
}
