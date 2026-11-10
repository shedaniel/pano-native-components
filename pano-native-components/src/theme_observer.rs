use tokio::sync::mpsc;

use crate::jni_callback::JniCallback;

pub async fn observe(
    callback_sender: mpsc::Sender<JniCallback>,
) -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(target_os = "linux")]
    {
        use ashpd::desktop::settings::{ColorScheme, Settings};
        use futures_util::StreamExt;

        let settings = match Settings::new().await {
            Ok(s) => s,
            Err(e) => {
                log::error!("Failed to connect to Settings XDG portal: {}", e);
                return Ok(());
            }
        };

        let scheme = match settings.color_scheme().await {
            Ok(s) => s,
            Err(e) => {
                log::error!("Failed to get color scheme: {}", e);
                return Ok(());
            }
        };

        let is_dark_mode = match scheme {
            ColorScheme::PreferDark => true,
            ColorScheme::PreferLight | ColorScheme::NoPreference => false,
        };
        let _ = callback_sender
            .send(JniCallback::DarkModeChanged(is_dark_mode))
            .await;

        let mut color_scheme_stream = match settings.receive_color_scheme_changed().await {
            Ok(s) => s,
            Err(e) => {
                log::error!("Failed to receive color scheme change stream: {}", e);
                return Ok(());
            }
        };

        while let Some(scheme) = color_scheme_stream.next().await {
            let is_dark_mode = match scheme {
                ColorScheme::PreferDark => true,
                ColorScheme::PreferLight | ColorScheme::NoPreference => false,
            };

            let _ = callback_sender
                .send(JniCallback::DarkModeChanged(is_dark_mode))
                .await;
        }
    }

    #[cfg(target_os = "windows")]
    {
        use windows::Win32::Foundation::HANDLE;
        use windows::Win32::System::Registry::RegGetValueW;
        use windows::Win32::System::Registry::{HKEY, RRF_RT_REG_DWORD};
        use windows::Win32::System::Registry::{
            HKEY_CURRENT_USER, KEY_READ, RegNotifyChangeKeyValue, RegOpenKeyExW,
        };
        use windows::Win32::System::Registry::{KEY_NOTIFY, REG_NOTIFY_CHANGE_LAST_SET};
        use windows::Win32::System::Threading::CreateEventW;
        use windows::Win32::{Foundation::CloseHandle, System::Registry::RegCloseKey};
        use windows::Win32::{
            Foundation::WAIT_OBJECT_0,
            System::Threading::{INFINITE, WaitForSingleObject},
        };
        use windows::core::w;

        fn read_dword_opt(hkey: HKEY, name: &windows::core::PCWSTR) -> Option<u32> {
            let mut data: u32 = 0;
            let mut size: u32 = std::mem::size_of::<u32>() as u32;

            // Subkey = null to read from hkey directly
            let result = unsafe {
                use std::ffi::c_void;

                RegGetValueW(
                    hkey,
                    None,
                    *name,
                    RRF_RT_REG_DWORD,
                    None,
                    Some((&mut data as *mut u32).cast::<c_void>()),
                    Some(&mut size),
                )
            };

            match result.ok() {
                Ok(()) => Some(data),
                Err(_) => None,
            }
        }

        tokio::task::spawn_blocking(move || {
            // Open the specific "Personalize" subkey
            let mut hkey = HKEY::default();
            let _ = unsafe {
                RegOpenKeyExW(
                    HKEY_CURRENT_USER,
                    w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
                    Some(0),
                    KEY_NOTIFY | KEY_READ,
                    &mut hkey,
                )
            };

            // Send initial state
            let apps_theme = read_dword_opt(hkey, &w!("AppsUseLightTheme"));
            let mut last_is_dark = apps_theme == Some(0);
            let _ = callback_sender.try_send(JniCallback::DarkModeChanged(last_is_dark));

            // Create an auto-reset event and arm the registry notification
            let h_event: HANDLE = unsafe { CreateEventW(None, false, false, None).unwrap() };
            let _ = unsafe {
                RegNotifyChangeKeyValue(
                    hkey,
                    false, // don't watch subkeys
                    REG_NOTIFY_CHANGE_LAST_SET,
                    Some(h_event),
                    true, // async; event is signaled on change
                )
            };

            // Wait for changes and re-arm after each notification
            loop {
                let wait = unsafe { WaitForSingleObject(h_event, INFINITE) };
                if wait != WAIT_OBJECT_0 {
                    break;
                }

                let apps_theme = read_dword_opt(hkey, &w!("AppsUseLightTheme"));
                let is_dark_mode = apps_theme == Some(0);
                if is_dark_mode != last_is_dark {
                    let _ = callback_sender.try_send(JniCallback::DarkModeChanged(is_dark_mode));
                    last_is_dark = is_dark_mode;
                }

                // Re-arm for the next change
                let _ = unsafe {
                    RegNotifyChangeKeyValue(
                        hkey,
                        false,
                        REG_NOTIFY_CHANGE_LAST_SET,
                        Some(h_event),
                        true,
                    )
                };
            }

            unsafe {
                let _ = RegCloseKey(hkey);
                let _ = CloseHandle(h_event);
            }
        })
        .await?;
    }

    #[cfg(target_os = "macos")]
    {
        // Phase 1: emit the current appearance once. Live observation is Phase 2.
        use std::process::Command;
        let is_dark = Command::new("defaults")
            .args(["read", "-g", "AppleInterfaceStyle"])
            .output()
            .map(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .to_lowercase()
                    .contains("dark")
            })
            .unwrap_or(false);
        let _ = callback_sender
            .send(JniCallback::DarkModeChanged(is_dark))
            .await;
    }

    Ok(())
}
