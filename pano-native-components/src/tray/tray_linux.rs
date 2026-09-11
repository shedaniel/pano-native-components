use crate::JniCallback;
use crate::tray::OUTGOING_TRAY_EVENT_TX;
use crate::tray::PanoTrayData;
use crate::tray::TRAY_DATA_TX;
use crate::tray::send_tray_click;
use ksni::{MenuItem, TrayMethods, menu::StandardItem};
use std::sync::OnceLock;
use tokio::sync::mpsc;

struct PanoTray {
    pub data: PanoTrayData,
}

impl ksni::Tray for PanoTray {
    fn id(&self) -> String {
        super::TRAY_NAME.to_string()
    }

    fn icon_name(&self) -> String {
        self.data.icon_name.clone()
    }

    fn icon_theme_path(&self) -> String {
        self.data.icons_dir.clone()
    }

    const MENU_ON_ACTIVATE: bool = true;

    fn title(&self) -> String {
        "Pano Scrobbler".to_string()
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        // title is the first line of the tooltip, description is the rest.
        // if the tooltip is only one line, the description will be empty
        // also handle the case where the tooltip is empty

        let mut lines = self.data.tooltip.trim().lines();
        let title = lines.next().unwrap_or("").to_string();
        let description = lines.collect::<Vec<&str>>().join("\n");

        ksni::ToolTip {
            title,
            description,
            ..Default::default()
        }
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        self.data
            .menu_items
            .iter()
            .map(|(id, text)| {
                let id_owned = id.clone();
                let text_owned = text.replace("_", "__"); // see ksni docs for why
                match id.as_str() {
                    "Separator" => MenuItem::Separator,
                    _ => MenuItem::Standard(StandardItem {
                        label: text_owned,
                        activate: Box::new(move |_tray| {
                            send_tray_click(id_owned.clone());
                        }),
                        enabled: !id.ends_with("Disabled"),
                        ..Default::default()
                    }),
                }
            })
            .collect()
    }
}

pub async fn tray_listener(
    callback_sender: mpsc::Sender<JniCallback>,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut receiver = TRAY_DATA_TX.subscribe();

    OUTGOING_TRAY_EVENT_TX.set(callback_sender).unwrap();

    let tray_handle: OnceLock<ksni::Handle<PanoTray>> = OnceLock::new();

    loop {
        let tray_data = receiver.borrow_and_update().clone();

        if let Some(tray_data) = tray_data {
            match tray_handle.get() {
                Some(handle) => {
                    handle
                        .update(|existing_tray| {
                            existing_tray.data = tray_data;
                        })
                        .await;
                }
                None => {
                    let tray = PanoTray { data: tray_data };
                    match tray.disable_dbus_name(ashpd::is_sandboxed()).spawn().await {
                        Ok(handle) => {
                            let _ = tray_handle.set(handle);
                        }
                        Err(e) => {
                            log::error!("Failed to spawn tray: {e}");
                        }
                    }
                }
            }
        }

        if receiver.changed().await.is_err() {
            break;
        }
    }

    Ok(())
}
