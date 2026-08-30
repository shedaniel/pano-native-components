use std::{collections::HashMap, str::FromStr, sync::OnceLock, time::Duration};

use ashpd::desktop::{
    Icon,
    notification::{DisplayHint, Notification, NotificationProxy, Priority},
    open_uri::{OpenFileOptions, OpenFileRequest, OpenURIProxy},
};
use futures_util::{TryFutureExt, stream::StreamExt};
use tokio::{
    sync::{
        RwLock, RwLockWriteGuard,
        mpsc::{self, Receiver, Sender},
    },
    task::JoinHandle,
    time::timeout,
};
use zbus::{
    Connection,
    fdo::{DBusProxy, NameOwnerChangedArgs},
    zvariant::{self},
};

use crate::{
    INCOMING_PLAYER_EVENT_TX, file_picker, ipc,
    jni_callback::JniCallback,
    media_events::{IncomingEvent, MetadataInfo, PlaybackInfo, PlaybackState, SessionInfo},
    media_listener::linux_mpris::{
        autostart, media_player2::MediaPlayer2Proxy, notifications::NotificationsProxy,
        player::PlayerProxy,
    },
    theme_observer,
};
use crate::{media_listener::linux_mpris::metadata::Metadata, tray};

const MPRIS2_PREFIX: &str = "org.mpris.MediaPlayer2.";

#[derive(Debug)]
pub struct ShutdownListenerError {}

impl std::error::Error for ShutdownListenerError {}

impl std::fmt::Display for ShutdownListenerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "MPRIS listener is shutting down")
    }
}

struct PlayerListenerHandle {
    join_handle: JoinHandle<zbus::Result<()>>,
    incoming_player_event_tx: Sender<IncomingEvent>,
}

async fn get_identity(connection: &Connection, dbus_name: &str) -> String {
    let media_player2_proxy = match MediaPlayer2Proxy::new(connection, dbus_name).await {
        Ok(proxy) => proxy,
        Err(_) => return String::new(),
    };

    // some chromium instances await forever for identity

    match timeout(Duration::from_millis(200), media_player2_proxy.identity()).await {
        Ok(Ok(id)) => id,
        _ => String::new(),
    }
}

static OUTGOING_PLAYER_EVENT_TX: OnceLock<mpsc::Sender<JniCallback>> = OnceLock::new();

#[tokio::main(flavor = "current_thread")]
pub async fn listener(
    jni_callback: impl Fn(JniCallback) -> Option<bool> + 'static,
) -> Result<(), Box<dyn std::error::Error>> {
    let (all_players_tx, mut all_players_rx) = mpsc::channel(100);

    let _ = all_players_tx.try_send(IncomingEvent::RefreshSessions);

    *INCOMING_PLAYER_EVENT_TX.lock().unwrap() = Some(all_players_tx);

    let (outgoing_tx, mut outgoing_rx) = mpsc::channel(10);
    OUTGOING_PLAYER_EVENT_TX.set(outgoing_tx.clone()).unwrap();

    let names_to_handles: RwLock<HashMap<String, PlayerListenerHandle>> =
        RwLock::new(HashMap::new());
    let dbus_names_to_identities: RwLock<HashMap<String, String>> = RwLock::new(HashMap::new());

    let connection = Connection::session().await?;

    let dbus_proxy = DBusProxy::new(&connection).await?;

    let is_app_allowed = |app_id: &str| -> bool {
        jni_callback(JniCallback::IsAppIdAllowed(app_id.to_string())).unwrap_or(false)
    };

    // listener just started, poll existing values

    let dbus_names = dbus_proxy.list_names().await;
    let dbus_names_filtered = match dbus_names {
        Ok(names) => names
            .into_iter()
            .filter(|name| name.starts_with(MPRIS2_PREFIX))
            .collect::<Vec<_>>(),
        Err(e) => {
            log::error!("Failed to list DBus names: {e}");
            Vec::new()
        }
    };

    for dbus_name in dbus_names_filtered {
        if !names_to_handles
            .read()
            .await
            .contains_key(dbus_name.as_str())
        {
            dbus_names_to_identities.write().await.insert(
                dbus_name.to_string(),
                get_identity(&connection, &dbus_name).await,
            );

            if is_app_allowed(&dbus_name) {
                start_tracking_player(
                    &connection,
                    dbus_name.to_string(),
                    &mut names_to_handles.write().await,
                );
            }
        }
    }

    // listen for new players
    let mut name_owner_changed = dbus_proxy.receive_name_owner_changed().await?;

    let incoming_events = async {
        while let Some(incoming_event) = all_players_rx.recv().await {
            match &incoming_event {
                IncomingEvent::Skip(app_id) => {
                    let names_to_handles = names_to_handles.read().await;
                    let handle = names_to_handles.get(app_id);

                    if let Some(handle) = handle {
                        let _ = handle.incoming_player_event_tx.send(incoming_event).await;
                    }
                }

                IncomingEvent::Mute(app_id) => {
                    let names_to_handles = names_to_handles.read().await;
                    let handle = names_to_handles.get(app_id);

                    if let Some(handle) = handle {
                        let _ = handle.incoming_player_event_tx.send(incoming_event).await;
                    }
                }

                IncomingEvent::Unmute(app_id) => {
                    let names_to_handles = names_to_handles.read().await;
                    let handle = names_to_handles.get(app_id);

                    if let Some(handle) = handle {
                        let _ = handle.incoming_player_event_tx.send(incoming_event).await;
                    }
                }

                IncomingEvent::RefreshSessions => {
                    for app_id in dbus_names_to_identities.read().await.keys() {
                        let is_allowed = is_app_allowed(app_id);
                        let is_tracking = names_to_handles.read().await.contains_key(app_id);
                        if is_allowed && !is_tracking {
                            start_tracking_player(
                                &connection,
                                app_id.to_string(),
                                &mut names_to_handles.write().await,
                            );
                        }

                        if !is_allowed && is_tracking {
                            stop_tracking_player(names_to_handles.write().await.remove(app_id));
                        }
                    }

                    let session_infos = dbus_names_to_identities
                        .read()
                        .await
                        .iter()
                        .map(|(dbus_name, identity)| SessionInfo {
                            app_id: dbus_name.clone(),
                            app_name: identity.clone(),
                        })
                        .collect::<Vec<SessionInfo>>();

                    jni_callback(JniCallback::SessionsChanged(session_infos));
                }

                IncomingEvent::Shutdown => {
                    for (_app_id, handle) in names_to_handles.write().await.drain() {
                        stop_tracking_player(Some(handle));
                    }

                    dbus_names_to_identities.write().await.clear();

                    // produce some error to stop the tasks
                    return Result::Err(ShutdownListenerError {});
                }

                IncomingEvent::LaunchFilePicker(
                    request_id,
                    _hwnd,
                    save,
                    title,
                    file_name,
                    extensions,
                ) => {
                    let filters = extensions
                        .iter()
                        .map(|x| format!("*{x}"))
                        .collect::<Vec<_>>();

                    let uri = file_picker::launch_file_picker(
                        *save,
                        title.clone(),
                        file_name.clone(),
                        filters,
                    )
                    .await;
                    let _ = OUTGOING_PLAYER_EVENT_TX
                        .get()
                        .unwrap()
                        .try_send(JniCallback::FilePicked(*request_id, uri));
                }

                IncomingEvent::AutoStart(add) => {
                    if ashpd::is_sandboxed() {
                        autostart::autostart_sandboxed(*add).await;
                    } else {
                        autostart::autostart(*add);
                    }
                }

                IncomingEvent::Notification(title, body) => {
                    if ashpd::is_sandboxed() {
                        let proxy = NotificationProxy::new().await;

                        if let Ok(proxy) = proxy {
                            let _ = proxy
                                .add_notification(
                                    "com.arn.scrobble",
                                    Notification::new(title)
                                        .body(body.as_str())
                                        .priority(Priority::Normal)
                                        .icon(Icon::with_names(["pano-scrobbler"]))
                                        .display_hint([DisplayHint::Tray]),
                                )
                                .await;
                        }
                    } else {
                        let proxy = NotificationsProxy::new(&connection).await;
                        if let Ok(proxy) = proxy {
                            let _ = proxy
                                .notify(
                                    "Pano Scrobbler",
                                    0,
                                    "pano-scrobbler",
                                    title,
                                    body,
                                    &[],
                                    HashMap::new(),
                                    10000,
                                )
                                .await;
                        }
                    }
                }

                IncomingEvent::OpenUrl(url) => {
                    let proxy = OpenURIProxy::new().await;
                    if let Ok(proxy) = proxy {
                        let uri = ashpd::Uri::parse(url).unwrap();

                        if url.starts_with("file://") {
                            let _ = OpenFileRequest::default().ask(true).send_uri(&uri).await;
                        } else {
                            let _ = proxy.open_uri(None, &uri, OpenFileOptions::default()).await;
                        }
                    }
                }
            }
        }

        Ok(())
    };
    let mpris_events = async {
        while let Some(name_owner_changed) = name_owner_changed.next().await {
            let name_owner_changed: NameOwnerChangedArgs = name_owner_changed.args()?;
            let dbus_name: &str = name_owner_changed.name();

            if !dbus_name.starts_with(MPRIS2_PREFIX) {
                continue;
            }

            let old_owner = name_owner_changed.old_owner();
            let new_owner = name_owner_changed.new_owner();

            // handle player added
            if old_owner.is_none()
                && new_owner.is_some()
                && !names_to_handles.read().await.contains_key(dbus_name)
            {
                dbus_names_to_identities.write().await.insert(
                    dbus_name.to_string(),
                    get_identity(&connection, dbus_name).await,
                );

                if is_app_allowed(dbus_name) {
                    start_tracking_player(
                        &connection,
                        dbus_name.to_string(),
                        &mut names_to_handles.write().await,
                    );
                }
            }

            // handle player removed
            if old_owner.is_some() && new_owner.is_none() {
                stop_tracking_player(names_to_handles.write().await.remove(dbus_name));

                // remove the entry from session_infos
                dbus_names_to_identities
                    .write()
                    .await
                    .retain(|name, _| *name != dbus_name);
            }

            let session_infos = dbus_names_to_identities
                .read()
                .await
                .iter()
                .map(|(dbus_name, identity)| SessionInfo {
                    app_id: dbus_name.clone(),
                    app_name: identity.clone(),
                })
                .collect::<Vec<SessionInfo>>();

            let _ = OUTGOING_PLAYER_EVENT_TX
                .get()
                .unwrap()
                .try_send(JniCallback::SessionsChanged(session_infos));
        }
        Ok::<(), zbus::Error>(())
    };

    // other listeners
    let ipc_commands = ipc::commands_listener(move |command: String, arg: String| {
        let event = JniCallback::IpcCallback(command, arg);
        let _ = OUTGOING_PLAYER_EVENT_TX.get().unwrap().try_send(event);
    });

    let tray = tray::tray_listener(outgoing_tx.clone());

    let outgoing_events = async {
        while let Some(event) = outgoing_rx.recv().await {
            jni_callback(event);
        }

        Ok(())
    };

    let theme_observer = theme_observer::observe(outgoing_tx);

    let exit_res = tokio::try_join!(
        incoming_events.map_err(Into::into),
        mpris_events.map_err(Into::into),
        ipc_commands,
        tray,
        outgoing_events,
        theme_observer,
    );

    match exit_res {
        Ok(_) => Ok(()),
        Err(e) => {
            if e.downcast_ref::<ShutdownListenerError>().is_some() {
                Ok(())
            } else {
                Err(e)
            }
        }
    }
}

async fn player_listeners(
    connection: Connection,
    app_id: String,
    mut incoming_player_event_rx: Receiver<IncomingEvent>,
) -> zbus::Result<()> {
    let player_proxy = PlayerProxy::builder(&connection)
        .uncached_properties(&["Position"])
        .destination(app_id.clone())?
        .build()
        .await?;

    // todo: handle errors

    let mut prev_volume = player_proxy.volume().await.unwrap_or_default();

    let metadata = player_proxy.metadata().await.unwrap_or_default();
    let metadata_event = parse_metadata(metadata);

    let _ = OUTGOING_PLAYER_EVENT_TX
        .get()
        .unwrap()
        .try_send(JniCallback::MetadataChanged(app_id.clone(), metadata_event));

    let playback_status = player_proxy.playback_status().await.unwrap_or_default();
    let can_go_next = player_proxy.can_go_next().await.unwrap_or_default();
    let position = player_proxy
        .position()
        .await
        .map(|x| x / 1000)
        .unwrap_or(-1);
    let playback_event = parse_playback_state(playback_status, can_go_next, position);

    let _ = OUTGOING_PLAYER_EVENT_TX
        .get()
        .unwrap()
        .try_send(JniCallback::PlaybackStateChanged(
            app_id.clone(),
            playback_event,
        ));

    let mut metadata_changed = player_proxy.receive_metadata_changed().await;
    let mut playback_status_changed = player_proxy.receive_playback_status_changed().await;
    let mut can_go_next_changed = player_proxy.receive_can_go_next_changed().await;
    let seek_changed = player_proxy.receive_seeked().await;
    // this does not work
    // let mut position_changed = player_proxy.receive_position_changed().await;

    let metadata_listener = async {
        while let Some(metadata_changed) = metadata_changed.next().await {
            let metadata = metadata_changed.get().await.unwrap_or_default();
            let metadata_event = parse_metadata(metadata);
            let _ = OUTGOING_PLAYER_EVENT_TX
                .get()
                .unwrap()
                .try_send(JniCallback::MetadataChanged(app_id.clone(), metadata_event));

            // re-fetch position for players with gapless playback
            let position = player_proxy
                .position()
                .await
                .map(|x| x / 1000)
                .unwrap_or(-1);
            let can_go_next = player_proxy.cached_can_go_next().unwrap_or_default();
            let playback_status = player_proxy.cached_playback_status().unwrap_or_default();

            // skip if not cached
            if playback_status.is_none() || can_go_next.is_none() {
                continue;
            }

            let playback_event =
                parse_playback_state(playback_status.unwrap(), can_go_next.unwrap(), position);

            let _ = OUTGOING_PLAYER_EVENT_TX.get().unwrap().try_send(
                JniCallback::PlaybackStateChanged(app_id.clone(), playback_event),
            );
        }

        zbus::Result::Ok(())
    };

    let position_listener = async {
        while let Some(playback_status_changed) = playback_status_changed.next().await {
            let playback_status = playback_status_changed.get().await.unwrap_or_default();
            let position = player_proxy
                .position()
                .await
                .map(|x| x / 1000)
                .unwrap_or(-1);
            let playback_event = parse_playback_state(
                playback_status,
                player_proxy
                    .cached_can_go_next()
                    .unwrap_or_default()
                    .unwrap_or_default(),
                position,
            );
            let _ = OUTGOING_PLAYER_EVENT_TX.get().unwrap().try_send(
                JniCallback::PlaybackStateChanged(app_id.clone(), playback_event),
            );
        }
        zbus::Result::Ok(())
    };

    let can_go_next_listener = async {
        while let Some(can_go_next_changed) = can_go_next_changed.next().await {
            let can_go_next = can_go_next_changed.get().await.unwrap_or_default();
            let position = player_proxy
                .position()
                .await
                .map(|x| x / 1000)
                .unwrap_or(-1);
            let playback_event = parse_playback_state(
                player_proxy
                    .cached_playback_status()
                    .unwrap_or_default()
                    .unwrap_or_default(),
                can_go_next,
                position,
            );
            let _ = OUTGOING_PLAYER_EVENT_TX.get().unwrap().try_send(
                JniCallback::PlaybackStateChanged(app_id.clone(), playback_event),
            );
        }
        zbus::Result::Ok(())
    };
    let seek_listener = async {
        if let Ok(mut seek_changed) = seek_changed {
            let debounce_duration = Duration::from_secs(1);

            let emit_position = |position: i64| {
                let playback_event = parse_playback_state(
                    player_proxy
                        .cached_playback_status()
                        .unwrap_or_default()
                        .unwrap_or_default(),
                    player_proxy
                        .cached_can_go_next()
                        .unwrap_or_default()
                        .unwrap_or_default(),
                    position,
                );
                let _ = OUTGOING_PLAYER_EVENT_TX.get().unwrap().try_send(
                    JniCallback::PlaybackStateChanged(app_id.clone(), playback_event),
                );
            };

            while let Some(seek_signal) = seek_changed.next().await {
                let Ok(seek_args) = seek_signal.args() else {
                    continue;
                };

                let mut latest_position = *seek_args.Position() / 1000;

                loop {
                    match timeout(debounce_duration, seek_changed.next()).await {
                        Ok(Some(next_signal)) => {
                            if let Ok(next_args) = next_signal.args() {
                                latest_position = *next_args.Position() / 1000;
                            }
                            continue;
                        }
                        Ok(None) => {
                            emit_position(latest_position);
                            return zbus::Result::Ok(());
                        }
                        Err(_) => {
                            emit_position(latest_position);
                            break;
                        }
                    }
                }
            }
        }
        zbus::Result::Ok(())
    };

    let incoming_events_listener = async {
        while let Some(incoming_event) = incoming_player_event_rx.recv().await {
            match &incoming_event {
                IncomingEvent::Skip(_) => {
                    let _ = player_proxy.next().await;
                }
                IncomingEvent::Mute(_) => {
                    prev_volume = player_proxy.volume().await.unwrap_or_default();
                    let _ = player_proxy.set_volume(0.0).await;
                }
                IncomingEvent::Unmute(_) => {
                    let _ = player_proxy.set_volume(prev_volume).await;
                }
                IncomingEvent::Shutdown => break,
                _ => {
                    // do nothing, handled by the main listener
                }
            }
        }

        zbus::Result::Ok(())
    };

    tokio::try_join!(
        metadata_listener,
        position_listener,
        can_go_next_listener,
        seek_listener,
        incoming_events_listener,
    )?;

    Ok(())
}

fn start_tracking_player(
    connection: &Connection,
    app_id: String,
    names_to_handles: &mut RwLockWriteGuard<'_, HashMap<String, PlayerListenerHandle>>,
) {
    let (tx, rx) = mpsc::channel::<IncomingEvent>(1);

    let join_handle = tokio::spawn(player_listeners(connection.clone(), app_id.clone(), rx));

    names_to_handles.insert(
        app_id,
        PlayerListenerHandle {
            join_handle,
            incoming_player_event_tx: tx,
        },
    );
}

fn stop_tracking_player(handle: Option<PlayerListenerHandle>) {
    if let Some(handle) = handle {
        handle.join_handle.abort();
    }
}

fn parse_metadata(metadata: HashMap<String, zvariant::OwnedValue>) -> MetadataInfo {
    let metadata = Metadata::from(metadata);

    let first_artist = metadata.artists().unwrap_or_default().first().cloned();
    let first_album_artist = metadata
        .album_artists()
        .unwrap_or_default()
        .first()
        .cloned();

    let art_url = metadata.art_url().take_if(|x| x.len() < 1000);

    MetadataInfo {
        title: metadata.title().unwrap_or_default().to_string(),
        artist: first_artist.unwrap_or_default(),
        album: metadata.album_name().unwrap_or_default().to_string(),
        album_artist: first_album_artist.unwrap_or_default(),
        track_number: metadata.track_number().unwrap_or_default(),
        duration: metadata
            .length()
            .map(|x| x.as_millis().try_into().unwrap_or(-1))
            .unwrap_or(-1),
        art_url: art_url.unwrap_or_default().to_string(),
        track_url: metadata.url().unwrap_or_default().to_string(),
    }
}

fn parse_playback_state(playback_status: String, can_go_next: bool, position: i64) -> PlaybackInfo {
    let playback_status = PlaybackState::from_str(&playback_status).unwrap_or(PlaybackState::Other);

    PlaybackInfo {
        state: playback_status,
        position,
        can_skip: can_go_next,
        can_change_channel: false,
    }
}
