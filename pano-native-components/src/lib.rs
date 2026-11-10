mod machine_uid;
mod media_events;
mod media_listener;

mod file_picker;
#[cfg(target_os = "linux")]
mod tray;

mod discord_rpc;
mod ipc;
mod jni_callback;
mod theme_observer;
mod windows_utils;

use ftail::Ftail;
use jni::sys::{jboolean, jint, jlong};

use jni::EnvUnowned;
use jni::jni_sig;
use jni::jni_str;
use jni::objects::{JByteArray, JClass, JObjectArray, JString};

use jni_callback::JniCallback;
use log::LevelFilter;
use media_events::IncomingEvent;
use media_listener::listener;
use tokio::sync::mpsc;

use std::env;
use std::sync::{LazyLock, Mutex};

use crate::discord_rpc::DiscordActivity;
use crate::media_events::{MetadataInfo, PlaybackInfo};

static INCOMING_PLAYER_EVENT_TX: LazyLock<Mutex<Option<mpsc::Sender<IncomingEvent>>>> =
    LazyLock::new(|| Mutex::new(None));

static HWND: LazyLock<Mutex<Option<i64>>> = LazyLock::new(|| Mutex::new(None));

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_arn_scrobble_PanoNativeComponents_refreshSessions(
    _env: EnvUnowned,
    _class: JClass,
) {
    send_incoming_event(IncomingEvent::RefreshSessions);
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_arn_scrobble_PanoNativeComponents_setLogFilePath(
    mut unowned_env: EnvUnowned,
    _class: JClass,
    path: JString,
) {
    unowned_env
        .with_env(|env| -> jni::errors::Result<()> {
            let path: String = path.mutf8_chars(env)?.into();
            let path = std::path::PathBuf::from(path);

            #[cfg(debug_assertions)]
            let level = LevelFilter::Debug;

            #[cfg(not(debug_assertions))]
            let level = LevelFilter::Error;

            Ftail::new()
                .console(level) // log to console
                .single_file(&path, true, level)
                .max_file_size(1)
                .init()
                .expect("Failed to initialize logger"); // initialize logger
            Ok(())
        })
        .resolve::<jni::errors::ThrowRuntimeExAndDefault>()
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_arn_scrobble_PanoNativeComponents_stopListeningMedia(
    _env: EnvUnowned,
    _class: JClass,
) {
    send_incoming_event(IncomingEvent::Shutdown);
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_arn_scrobble_PanoNativeComponents_setEnvironmentVariable(
    mut unowned_env: EnvUnowned,
    _class: JClass,
    key: JString,
    value: JString,
) {
    unowned_env
        .with_env(|env| -> jni::errors::Result<()> {
            let key: String = key.mutf8_chars(env)?.into();
            let value: String = value.mutf8_chars(env)?.into();
            unsafe {
                env::set_var(key, value);
            }
            Ok(())
        })
        .resolve::<jni::errors::ThrowRuntimeExAndDefault>()
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_arn_scrobble_PanoNativeComponents_skip(
    mut unowned_env: EnvUnowned,
    _class: JClass,
    app_id: JString,
) {
    unowned_env
        .with_env(|env| -> jni::errors::Result<()> {
            let app_id: String = app_id.mutf8_chars(env)?.into();
            send_incoming_event(IncomingEvent::Skip(app_id));
            Ok(())
        })
        .resolve::<jni::errors::ThrowRuntimeExAndDefault>()
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_arn_scrobble_PanoNativeComponents_mute(
    mut unowned_env: EnvUnowned,
    _class: JClass,
    app_id: JString,
) {
    unowned_env
        .with_env(|env| -> jni::errors::Result<()> {
            let app_id: String = app_id.mutf8_chars(env)?.into();
            send_incoming_event(IncomingEvent::Mute(app_id));
            Ok(())
        })
        .resolve::<jni::errors::ThrowRuntimeExAndDefault>()
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_arn_scrobble_PanoNativeComponents_unmute(
    mut unowned_env: EnvUnowned,
    _class: JClass,
    app_id: JString,
) {
    unowned_env
        .with_env(|env| -> jni::errors::Result<()> {
            let app_id: String = app_id.mutf8_chars(env)?.into();
            send_incoming_event(IncomingEvent::Unmute(app_id));
            Ok(())
        })
        .resolve::<jni::errors::ThrowRuntimeExAndDefault>()
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_arn_scrobble_PanoNativeComponents_notify(
    mut unowned_env: EnvUnowned,
    _class: JClass,
    title: JString,
    body: JString,
) {
    unowned_env
        .with_env(|env| -> jni::errors::Result<()> {
            let title: String = title.mutf8_chars(env)?.into();
            let body: String = body.mutf8_chars(env)?.into();
            send_incoming_event(IncomingEvent::Notification(title, body));
            Ok(())
        })
        .resolve::<jni::errors::ThrowRuntimeExAndDefault>()
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_arn_scrobble_PanoNativeComponents_setTrayLinux(
    mut unowned_env: EnvUnowned,
    _class: JClass,
    tooltip: JString,
    png_bytes: JByteArray,
    invert: jboolean,
    menu_item_ids: JObjectArray<JString>,
    menu_item_texts: JObjectArray<JString>,
) {
    #[cfg(target_os = "linux")]
    {
        unowned_env
            .with_env(|env| -> jni::errors::Result<()> {
                use jni::objects::ReleaseMode;

                use crate::tray::{PanoTrayData, update_tray};

                let tooltip: String = tooltip.mutf8_chars(env)?.into();

                let png_bytes_rust =
                    unsafe { png_bytes.get_elements(env, ReleaseMode::NoCopyBack) }?.to_vec();
                // convert vec<i8> to vec<u8>
                let png_bytes_rust = png_bytes_rust.into_iter().map(|b| b as u8).collect();

                let len = menu_item_ids.len(env)?;
                let mut menu_items = Vec::<(String, String)>::with_capacity(len);

                for i in 0..len {
                    let id = menu_item_ids.get_element(env, i)?.to_string();
                    let text = menu_item_texts.get_element(env, i)?.to_string();

                    menu_items.push((id, text));
                }

                update_tray(PanoTrayData {
                    tooltip,
                    png_bytes: png_bytes_rust,
                    invert,
                    menu_items,
                });
                Ok(())
            })
            .resolve::<jni::errors::ThrowRuntimeExAndDefault>();
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_arn_scrobble_PanoNativeComponents_getMachineId<'local>(
    mut unowned_env: EnvUnowned<'local>,
    _class: JClass<'local>,
) -> JString<'local> {
    unowned_env
        .with_env(|env| -> jni::errors::Result<JString<'_>> {
            let id = machine_uid::get().unwrap();
            JString::from_str(env, id)
        })
        .resolve::<jni::errors::ThrowRuntimeExAndDefault>()
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_arn_scrobble_PanoNativeComponents_setHwndWindows(
    _env: EnvUnowned,
    _class: JClass,
    handle: jlong,
) {
    #[cfg(target_os = "windows")]
    {
        let mut hwnd = HWND.lock().unwrap();
        *hwnd = Some(handle);
        windows_utils::apply_dark_mode_to_window(handle);
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_arn_scrobble_PanoNativeComponents_sendIpcCommand(
    mut unowned_env: EnvUnowned,
    _class: JClass,
    command: JString,
    arg: JString,
) -> jboolean {
    unowned_env
        .with_env(|env| -> jni::errors::Result<jboolean> {
            let command: String = command.mutf8_chars(env)?.into();
            let arg: String = arg.mutf8_chars(env)?.into();
            Ok(match ipc::send_command(&command, &arg) {
                Ok(_) => true,
                Err(e) => {
                    if command != "focus-existing" {
                        log::error!("Error sending ipc command: {e}");
                    }
                    false
                }
            })
        })
        .resolve::<jni::errors::ThrowRuntimeExAndDefault>()
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_arn_scrobble_PanoNativeComponents_isFileLockedWindows(
    mut unowned_env: EnvUnowned,
    _class: JClass,
    path: JString,
) -> jboolean {
    #[cfg(target_os = "windows")]
    {
        unowned_env
            .with_env(|env| -> jni::errors::Result<jboolean> {
                let path: String = path.mutf8_chars(env)?.into();
                Ok(windows_utils::is_file_locked(&path))
            })
            .resolve::<jni::errors::ThrowRuntimeExAndDefault>()
    }

    #[cfg(not(target_os = "windows"))]
    {
        false
    }
}

pub fn send_incoming_event(incoming_event: IncomingEvent) {
    let tx = INCOMING_PLAYER_EVENT_TX.lock().unwrap();

    log::debug!("Sending message: {:?}", &incoming_event);

    if let Some(ref sender) = *tx {
        match sender.try_send(incoming_event) {
            Ok(_) => {}
            Err(e) => log::error!("Error sending incoming event: {e}"),
        }
    } else {
        log::error!("Sender not initialized, did not send {incoming_event:?}");
    }
}

fn call_java_fn(env: &mut jni::Env, event: &JniCallback) -> Option<bool> {
    let class = jni_str!("com/arn/scrobble/PanoNativeComponents");

    let result = match event {
        JniCallback::SessionsChanged(session_infos) => {
            // Create a Java String array
            let app_ids =
                JObjectArray::<JString>::new(env, session_infos.len(), JString::null()).unwrap();
            let app_names =
                JObjectArray::<JString>::new(env, session_infos.len(), JString::null()).unwrap();

            // Populate the array
            for (i, session_info) in session_infos.iter().enumerate() {
                let j_app_id = JString::from_str(env, &session_info.app_id).unwrap();
                app_ids.set_element(env, i, j_app_id).unwrap();

                let j_app_name = JString::from_str(env, &session_info.app_name).unwrap();
                app_names.set_element(env, i, j_app_name).unwrap();
            }

            env.call_static_method(
                class,
                jni_str!("onActiveSessionsChanged"),
                jni_sig!("([Ljava/lang/String;[Ljava/lang/String;)V"),
                &[(&app_ids).into(), (&app_names).into()],
            )
        }

        JniCallback::MetadataChanged(
            app_id,
            MetadataInfo {
                title,
                artist,
                album,
                album_artist,
                track_number,
                duration,
                art_url,
                track_url,
            },
        ) => {
            let app_id = JString::from_str(env, app_id).unwrap();
            let track_url = JString::from_str(env, track_url).unwrap();
            let title = JString::from_str(env, title).unwrap();
            let artist = JString::from_str(env, artist).unwrap();
            let album = JString::from_str(env, album).unwrap();
            let album_artist = JString::from_str(env, album_artist).unwrap();
            let art_url = JString::from_str(env, art_url).unwrap();
            env.call_static_method(
                class,
                jni_str!("onMetadataChanged"),
                jni_sig!("(Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;IJLjava/lang/String;Ljava/lang/String;)V"),
                &[(&app_id).into(), (&title).into(), (&artist).into(), (&album).into(), (&album_artist).into(), (*track_number).into(), (*duration).into(), (&art_url).into(), (&track_url).into(),],
            )
        }
        JniCallback::PlaybackStateChanged(
            app_id,
            PlaybackInfo {
                state,
                position,
                can_skip,
            },
        ) => {
            let app_id = JString::from_str(env, app_id).unwrap();
            let state = JString::from_str(env, state.to_string()).unwrap();
            env.call_static_method(
                class,
                jni_str!("onPlaybackStateChanged"),
                jni_sig!("(Ljava/lang/String;Ljava/lang/String;JZ)V"),
                &[
                    (&app_id).into(),
                    (&state).into(),
                    (*position).into(),
                    (*can_skip).into(),
                ],
            )
        }

        JniCallback::IpcCallback(command, arg) => {
            let command = JString::from_str(env, command).unwrap();
            let arg = JString::from_str(env, arg).unwrap();
            env.call_static_method(
                class,
                jni_str!("onReceiveIpcCommand"),
                jni_sig!("(Ljava/lang/String;Ljava/lang/String;)V"),
                &[(&command).into(), (&arg).into()],
            )
        }

        #[cfg(target_os = "linux")]
        JniCallback::TrayItemClicked(item_id) => {
            let item_id = JString::from_str(env, item_id).unwrap();
            env.call_static_method(
                class,
                jni_str!("onTrayMenuItemClicked"),
                jni_sig!("(Ljava/lang/String;)V"),
                &[(&item_id).into()],
            )
        }

        JniCallback::FilePicked(req_id, uri) => {
            let uri = JString::from_str(env, uri).unwrap();

            env.call_static_method(
                class,
                jni_str!("onFilePicked"),
                jni_sig!("(ILjava/lang/String;)V"),
                &[(*req_id).into(), (&uri).into()],
            )
        }

        JniCallback::DarkModeChanged(is_dark_mode) => env.call_static_method(
            class,
            jni_str!("onDarkModeChange"),
            jni_sig!("(Z)V"),
            &[(*is_dark_mode).into()],
        ),

        JniCallback::IsAppIdAllowed(app_id) => {
            let app_id_j = JString::from_str(env, app_id).unwrap();
            env.call_static_method(
                class,
                jni_str!("isAppIdAllowed"),
                jni_sig!("(Ljava/lang/String;)Z"),
                &[(&app_id_j).into()],
            )
        }
    };

    if let Err(e) = result {
        log::error!("Error calling java method: {e}");
    } else if let Ok(ret_val) = result
        && let JniCallback::IsAppIdAllowed(_) = event
    {
        return Some(ret_val.z().unwrap_or(false));
    }

    None
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_arn_scrobble_PanoNativeComponents_startListeningMedia(
    mut unowned_env: EnvUnowned,
    _class: JClass,
) {
    unowned_env
        .with_env(|env| -> jni::errors::Result<()> {
            let jvm = env.get_java_vm()?;

            if let Err(e) = listener(move |event| -> Option<bool> {
                jvm.attach_current_thread(|env| -> jni::errors::Result<Option<bool>> {
                    Ok(call_java_fn(env, &event))
                })
                .unwrap_or(None)
            }) {
                log::error!("Error listening for media: {e}");
            }
            Ok(())
        })
        .resolve::<jni::errors::ThrowRuntimeExAndDefault>()
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_arn_scrobble_PanoNativeComponents_updateDiscordActivity(
    mut unowned_env: EnvUnowned,
    _class: JClass,
    client_id: JString,
    name: JString,
    state: JString,
    details: JString,
    large_text: JString,
    start_time: jlong,
    end_time: jlong,
    art_url: JString,
    details_url: JString,
    is_playing: jboolean,
    status_line: jint,
    button_text: JString,
    button_url: JString,
) -> jboolean {
    unowned_env
        .with_env(|env| -> jni::errors::Result<jboolean> {
            let client_id: String = client_id.mutf8_chars(env)?.into();
            let name: String = name.mutf8_chars(env)?.into();
            let state: String = state.mutf8_chars(env)?.into();
            let details: String = details.mutf8_chars(env)?.into();
            let large_text: String = large_text.mutf8_chars(env)?.into();
            let art_url: String = art_url.mutf8_chars(env)?.into();
            let details_url: String = details_url.mutf8_chars(env)?.into();
            let button_text: String = button_text.mutf8_chars(env)?.into();
            let button_url: String = button_url.mutf8_chars(env)?.into();

            let end_time = if end_time > 0 { Some(end_time) } else { None };

            let activity = DiscordActivity {
                client_id,
                name,
                state,
                details,
                large_text,
                start_time,
                end_time,
                art_url,
                details_url,
                status_line,
                is_playing,
                button_text,
                button_url,
            };

            Ok(discord_rpc::update(activity).is_ok())
        })
        .resolve::<jni::errors::ThrowRuntimeExAndDefault>()
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_arn_scrobble_PanoNativeComponents_clearDiscordActivity(
    _env: EnvUnowned,
    _class: JClass,
    shutdown: jboolean,
) -> jboolean {
    discord_rpc::clear(shutdown).is_ok()
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_arn_scrobble_PanoNativeComponents_fileChooser(
    mut unowned_env: EnvUnowned,
    _class: JClass,
    request_id: jint,
    save: jboolean,
    title: JString,
    file_name: JString,
    filters: JObjectArray<JString>,
) {
    unowned_env
        .with_env(|env| -> jni::errors::Result<()> {
            let title: String = title.mutf8_chars(env)?.into();
            let file_name: String = file_name.mutf8_chars(env)?.into();
            let mut extensions_vec = Vec::new();

            for i in 0..filters.len(env)? {
                let filter = filters.get_element(env, i)?.to_string();
                extensions_vec.push(filter);
            }

            let hwnd = (*HWND.lock().unwrap()).unwrap_or_default();

            let event = IncomingEvent::LaunchFilePicker(
                request_id,
                hwnd,
                save,
                title,
                file_name,
                extensions_vec,
            );
            send_incoming_event(event);
            Ok(())
        })
        .resolve::<jni::errors::ThrowRuntimeExAndDefault>();
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_arn_scrobble_PanoNativeComponents_autoStartLinux(
    _env: EnvUnowned,
    _class: JClass,
    add: jboolean,
) {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        let event = IncomingEvent::AutoStart(add);
        send_incoming_event(event);
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_arn_scrobble_PanoNativeComponents_openUrl(
    mut unowned_env: EnvUnowned,
    _class: JClass,
    url: JString,
) {
    unowned_env
        .with_env(|env| -> jni::errors::Result<()> {
            let url: String = url.mutf8_chars(env)?.into();

            send_incoming_event(IncomingEvent::OpenUrl(url));

            Ok(())
        })
        .resolve::<jni::errors::ThrowRuntimeExAndDefault>();
}
