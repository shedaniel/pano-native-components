#[derive(Clone)]
pub struct PanoTrayData {
    pub tooltip: String,
    pub icons_dir: String,
    pub icon_name: String,
    pub menu_items: Vec<(String, String)>,
}

pub const TRAY_NAME: &str = "com.arn.scrobble.tray";

// created eagerly so update_tray() never races with tray_listener's startup
static TRAY_DATA_TX: LazyLock<watch::Sender<Option<PanoTrayData>>> =
    LazyLock::new(|| watch::channel(None).0);
static OUTGOING_TRAY_EVENT_TX: OnceLock<mpsc::Sender<JniCallback>> = OnceLock::new();

pub fn update_tray(pano_tray_data: PanoTrayData) {
    // send_replace always stores the value, even before any receiver subscribes
    TRAY_DATA_TX.send_replace(Some(pano_tray_data));
}

pub fn send_tray_click(id: impl Into<String>) {
    if let Some(tx) = OUTGOING_TRAY_EVENT_TX.get() {
        let _ = tx.try_send(JniCallback::TrayItemClicked(id.into()));
    }
}

#[cfg(target_os = "linux")]
mod tray_linux;
use std::sync::{LazyLock, OnceLock};

use tokio::sync::{mpsc, watch};
#[cfg(target_os = "linux")]
pub use tray_linux::tray_listener;

#[cfg(target_os = "windows")]
mod tray_windows;
#[cfg(target_os = "windows")]
pub use tray_windows::tray_listener;

use crate::jni_callback::JniCallback;
