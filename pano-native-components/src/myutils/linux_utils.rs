use ashpd::desktop::settings::{ColorScheme, Settings};
use futures_util::StreamExt;
use tokio::sync::mpsc;
use x11rb::{
    connection::Connection,
    protocol::xproto::{AtomEnum, ConnectionExt, PropMode},
    wrapper::ConnectionExt as _,
};

use crate::jni_callback::JniCallback;

pub fn apply_blur_to_window(window_id: u32) -> Result<(), Box<dyn std::error::Error>> {
    let (conn, _screen_num) = x11rb::connect(None)?;

    let atom = conn
        .intern_atom(false, b"_KDE_NET_WM_BLUR_BEHIND_REGION")?
        .reply()?
        .atom;

    let empty: [u32; 0] = [];

    let mut last_err: Option<Box<dyn std::error::Error>> = None;
    if conn.get_geometry(window_id)?.reply().is_ok() {
        // `.check()` forces a round trip so a BadWindow/protocol error
        // comes back as an Err instead of being silently dropped
        let result = conn
            .change_property32(
                PropMode::REPLACE,
                window_id,
                atom,
                AtomEnum::CARDINAL,
                &empty,
            )?
            .check();

        if result.is_ok() {
            conn.flush()?;
            return Ok(());
        }
        last_err = result.err().map(|e| e.into());
    }

    Err(last_err.unwrap_or_else(|| "failed to apply blur".into()))
}

pub async fn observe_theme_changes(
    callback_sender: mpsc::Sender<JniCallback>,
) -> Result<(), Box<dyn std::error::Error>> {
    {
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
    Ok(())
}
