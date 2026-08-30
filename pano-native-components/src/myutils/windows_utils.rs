use std::fs::OpenOptions;
use std::os::windows::fs::OpenOptionsExt;

use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Dwm::{
    DWMSBT_MAINWINDOW, DWMSBT_TRANSIENTWINDOW, DWMWA_SYSTEMBACKDROP_TYPE,
    DWMWA_USE_IMMERSIVE_DARK_MODE, DwmExtendFrameIntoClientArea, DwmSetWindowAttribute,
};
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows::Win32::UI::Controls::MARGINS;
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};
use windows_core::{PCSTR, w};
use windows_version::OsVersion;

pub fn apply_dark_mode_to_window(handle: i64, is_dark: bool) {
    let use_dark: i32 = if is_dark { 1 } else { 0 };
    let hwnd = HWND(handle as _);

    unsafe {
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            &use_dark as *const _ as _,
            std::mem::size_of_val(&use_dark) as u32,
        )
        .unwrap_or_else(|e| {
            log::error!("Failed to set DWMWA_USE_IMMERSIVE_DARK_MODE for window {handle}: {e}");
        });
    }
}

pub fn apply_blur_to_window(handle: i64, acryllic: bool) {
    // Windows 11 21H2 is build 10.0.22000
    if OsVersion::current() < OsVersion::new(10, 0, 0, 22000) {
        return;
    }

    let hwnd = HWND(handle as _);
    let backdrop_type = if acryllic {
        DWMSBT_TRANSIENTWINDOW
    } else {
        DWMSBT_MAINWINDOW
    };
    let margins = MARGINS {
        cxLeftWidth: -1,
        cxRightWidth: -1,
        cyTopHeight: -1,
        cyBottomHeight: -1,
    };
    // let corner_pref = DWMWCP_ROUND;

    unsafe {
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_SYSTEMBACKDROP_TYPE,
            &backdrop_type as *const _ as *const _,
            std::mem::size_of_val(&backdrop_type) as u32,
        )
        .unwrap_or_else(|e| {
            log::error!("Failed to set DWMWA_SYSTEMBACKDROP_TYPE: {e}");
        });

        // Extend the frame into the entire client area so the backdrop
        // is actually visible through it, not just at the (nonexistent) border.
        DwmExtendFrameIntoClientArea(hwnd, &margins).unwrap_or_else(|e| {
            log::error!("Failed to extend frame into client area: {e}");
        });

        // DwmSetWindowAttribute(
        //     hwnd,
        //     DWMWA_WINDOW_CORNER_PREFERENCE,
        //     &corner_pref as *const _ as *const _,
        //     std::mem::size_of::<i32>() as u32,
        // )
        // .unwrap_or_else(|e| {
        //     log::error!("Failed to set DWMWA_WINDOW_CORNER_PREFERENCE: {e}");
        // });
    }
}

pub fn is_file_locked(path: &str) -> bool {
    let file = OpenOptions::new()
        .create_new(false)
        .write(false)
        .read(true)
        .share_mode(0)
        .open(path);

    match file {
        Ok(_) => false,
        Err(err) => {
            // log::error!("Error opening file '{path}': {err}");
            err.raw_os_error() == Some(32) // ERROR_SHARING_VIOLATION
        }
    }
}

pub fn set_process_flags() {
    // Ordinals from uxtheme.dll (undocumented, stable since Win10 1903)
    const ORD_SET_PREFERRED_APP_MODE: usize = 135;
    type FnSetPreferredAppMode = unsafe extern "system" fn(i32) -> i32;

    unsafe {
        if let Ok(uxtheme) = LoadLibraryW(w!("uxtheme.dll"))
            && let Some(set_preferred_app_mode) =
                GetProcAddress(uxtheme, PCSTR(ORD_SET_PREFERRED_APP_MODE as _))
        {
            let set_preferred_app_mode: FnSetPreferredAppMode =
                std::mem::transmute(set_preferred_app_mode);
            // PreferredAppMode::AllowDark = 1
            set_preferred_app_mode(1);
        }

        // Tray menu looks blurry otherwise
        if let Err(e) = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) {
            log::error!("Failed to set process DPI awareness: {e}");
        }
    }
}
