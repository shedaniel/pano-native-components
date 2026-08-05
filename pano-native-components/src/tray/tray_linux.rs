use crate::JniCallback;
use crate::tray::OUTGOING_TRAY_EVENT_TX;
use crate::tray::PanoTrayData;
use crate::tray::TOKIO_USER_EVENT_SENDER;
use image::GenericImageView;
use ksni::{Icon, MenuItem, TrayMethods, menu::StandardItem};
use std::{
    hash::{DefaultHasher, Hash, Hasher},
    sync::OnceLock,
};
use tokio::sync::mpsc;

struct PanoTray {
    pub data: PanoTrayData,
    pub prev_icon_hash: u64,
    pub prev_icon: Option<Icon>,
}

fn compute_icon(png_bytes: &[u8], invert: bool) -> Option<Icon> {
    let img = image::load_from_memory_with_format(png_bytes, image::ImageFormat::Png);
    if let Ok(mut img) = img {
        if invert {
            img.invert();
        }
        let (width, height) = img.dimensions();
        let mut data = img.into_rgba8().into_vec();
        for pixel in data.chunks_exact_mut(4) {
            pixel.rotate_right(1); // rgba to argb
        }
        Some(Icon {
            width: width as i32,
            height: height as i32,
            data,
        })
    } else {
        log::error!("invalid png");
        None
    }
}

impl ksni::Tray for PanoTray {
    fn id(&self) -> String {
        "com.arn.scrobble.tray".into()
    }

    fn icon_pixmap(&self) -> Vec<Icon> {
        match &self.prev_icon {
            Some(icon) => vec![icon.clone()],
            None => vec![],
        }
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
                            OUTGOING_TRAY_EVENT_TX
                                .get()
                                .unwrap()
                                .try_send(JniCallback::TrayItemClicked(id_owned.clone()))
                                .unwrap();
                        }),
                        enabled: !id.ends_with("Disabled"),
                        ..Default::default()
                    }),
                }
            })
            .collect()
    }
}

fn compute_icon_hash(png_bytes: &[u8], invert: bool) -> u64 {
    let mut hasher = DefaultHasher::new();
    png_bytes.hash(&mut hasher);
    invert.hash(&mut hasher);
    hasher.finish()
}

pub async fn tray_listener(
    callback_sender: mpsc::Sender<JniCallback>,
) -> Result<(), Box<dyn std::error::Error>> {
    let (sender, mut receiver) = mpsc::channel::<PanoTrayData>(10);
    TOKIO_USER_EVENT_SENDER.set(sender).unwrap();

    OUTGOING_TRAY_EVENT_TX.set(callback_sender).unwrap();

    let tray_handle: OnceLock<ksni::Handle<PanoTray>> = OnceLock::new();
    let mut tray_init_attempted = false;

    while let Some(tray_data) = receiver.recv().await {
        if !tray_init_attempted {
            tray_init_attempted = true;

            let icon_hash = compute_icon_hash(&tray_data.png_bytes, tray_data.invert);
            let icon = compute_icon(&tray_data.png_bytes, tray_data.invert);

            let tray = PanoTray {
                data: tray_data,
                prev_icon_hash: icon_hash,
                prev_icon: icon,
            };
            match tray.disable_dbus_name(ashpd::is_sandboxed()).spawn().await {
                Ok(handle) => {
                    let _ = tray_handle.set(handle);
                }
                Err(e) => {
                    log::error!("Failed to spawn tray: {e}");
                }
            }
        } else if let Some(handle) = tray_handle.get() {
            handle
                .update(|existing_tray| {
                    let icon_hash = compute_icon_hash(&tray_data.png_bytes, tray_data.invert);
                    let icon = compute_icon(&tray_data.png_bytes, tray_data.invert);

                    if icon_hash != existing_tray.prev_icon_hash && icon.is_some() {
                        existing_tray.prev_icon = icon;
                        existing_tray.prev_icon_hash = icon_hash;
                    }
                    existing_tray.data = tray_data;
                })
                .await;
        }
    }

    Ok(())
}
