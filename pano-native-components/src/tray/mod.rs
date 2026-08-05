pub struct PanoTrayData {
    pub tooltip: String,
    pub png_bytes: Vec<u8>,
    pub invert: bool,
    pub menu_items: Vec<(String, String)>,
}

static TOKIO_USER_EVENT_SENDER: OnceLock<mpsc::Sender<PanoTrayData>> = OnceLock::new();
static OUTGOING_TRAY_EVENT_TX: OnceLock<mpsc::Sender<JniCallback>> = OnceLock::new();

pub fn update_tray(pano_tray_data: PanoTrayData) {
    if let Some(sender) = TOKIO_USER_EVENT_SENDER.get() {
        sender.try_send(pano_tray_data).unwrap_or_else(|_| {
            log::error!("Failed to send tray event");
        });
    } else {
        log::error!("Event loop not running");
    }
}

#[cfg(target_os = "linux")]
mod tray_linux;
use std::sync::OnceLock;

use tokio::sync::mpsc;
#[cfg(target_os = "linux")]
pub use tray_linux::tray_listener;

#[cfg(target_os = "windows")]
mod tray_windows;
#[cfg(target_os = "windows")]
pub use tray_windows::tray_listener;

use crate::jni_callback::JniCallback;
