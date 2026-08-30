use crate::file_picker::launch_file_picker;
use crate::jni_callback::JniCallback;
use crate::media_events::{IncomingEvent, MetadataInfo, PlaybackInfo, PlaybackState, SessionInfo};
use crate::{INCOMING_PLAYER_EVENT_TX, tray};
use crate::{ipc, theme_observer};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;
use tokio::sync::mpsc;
use windows::ApplicationModel::AppInfo;
use windows::Data::Xml::Dom::XmlDocument;
use windows::Foundation::TypedEventHandler;
use windows::Foundation::Uri;
use windows::Media::Control::{
    GlobalSystemMediaTransportControlsSession, GlobalSystemMediaTransportControlsSessionManager,
    GlobalSystemMediaTransportControlsSessionPlaybackStatus, MediaPropertiesChangedEventArgs,
    PlaybackInfoChangedEventArgs, SessionsChangedEventArgs, TimelinePropertiesChangedEventArgs,
};
use windows::System::Launcher;
use windows::UI::Notifications::ToastNotification;
use windows::UI::Notifications::ToastNotificationManager;
use windows_core::HSTRING;

// static ALBUM_ART_ENABLED: LazyLock<Mutex<bool>> = LazyLock::new(|| Mutex::new(false));

// const MAX_ART_BYTES: usize = 1024 * 1024; // 1 MB

// based on https://github.com/KDE/kdeconnect-kde/blob/master/plugins/mpriscontrol/mpriscontrolplugin-win.cpp

static OUTGOING_PLAYER_EVENT_TX: OnceLock<mpsc::Sender<JniCallback>> = OnceLock::new();

fn send_outgoing_event(outgoing_event: JniCallback) {
    let tx = OUTGOING_PLAYER_EVENT_TX.get();

    log::debug!("Sending outgoing message: {:?}", outgoing_event);

    if let Some(sender) = tx {
        match sender.try_send(outgoing_event) {
            Ok(_) => {}
            Err(e) => log::error!("Error sending outgoing event: {e}"),
        }
    } else {
        log::error!("Sender not initialized, did not send {outgoing_event:?}");
    }
}

#[tokio::main(flavor = "current_thread")]
pub async fn listener(
    jni_callback: impl Fn(JniCallback) -> Option<bool> + 'static,
) -> Result<(), Box<dyn std::error::Error>> {
    let (incoming_tx, mut incoming_rx) = mpsc::channel(10);
    let incoming_tx_clone = incoming_tx.clone();
    let _ = incoming_tx.try_send(IncomingEvent::RefreshSessions);
    *INCOMING_PLAYER_EVENT_TX.lock().unwrap() = Some(incoming_tx);

    // there is a lot of event spam on Windows, use a larger buffer
    let (outgoing_tx, mut outgoing_rx) = mpsc::channel(100);
    let outgoing_tx_clone = outgoing_tx.clone();
    let outgoing_tx_clone2 = outgoing_tx.clone();

    OUTGOING_PLAYER_EVENT_TX.set(outgoing_tx).unwrap();

    let mut app_names_cache = HashMap::<String, String>::new();
    let mut session_trackers: HashMap<String, SessionTracker> = HashMap::new();

    let is_app_allowed = |app_id: &str| -> bool {
        jni_callback(JniCallback::IsAppIdAllowed(app_id.to_string())).unwrap_or(false)
    };

    let manager = GlobalSystemMediaTransportControlsSessionManager::RequestAsync()?.join()?;

    let sessions_changed_token = manager
        .SessionsChanged(&TypedEventHandler::<
            GlobalSystemMediaTransportControlsSessionManager,
            SessionsChangedEventArgs,
        >::new(move |m, _args| {
            match m.as_ref() {
                Some(_) => {
                    let _ = incoming_tx_clone.try_send(IncomingEvent::RefreshSessions);
                }
                None => {
                    log::error!("SessionsChanged event handler received None");
                }
            }
            Ok(())
        }))
        .expect("Failed to set SessionsChanged event handler");

    let session_events = async {
        while let Some(event) = incoming_rx.recv().await {
            match event {
                IncomingEvent::Skip(app_id) => {
                    for tracker in session_trackers.values() {
                        let session_app_id = session_id(&tracker.session);

                        if session_app_id == app_id {
                            let _ = tracker.session.TrySkipNextAsync();
                            break;
                        }
                    }
                }
                IncomingEvent::RefreshSessions => {
                    update_sessions(
                        &manager,
                        &mut session_trackers,
                        &mut app_names_cache,
                        is_app_allowed,
                    );
                }

                IncomingEvent::Shutdown => {
                    INCOMING_PLAYER_EVENT_TX.lock().unwrap().take();
                    let _ = manager.RemoveSessionsChanged(sessions_changed_token);

                    // clear caches
                    app_names_cache.clear();
                    session_trackers.clear();

                    break;
                }

                IncomingEvent::Notification(title, body) => {
                    if let Err(e) = show_notification(&title, &body) {
                        log::error!("Error showing notification: {e:?}");
                    }
                }

                IncomingEvent::LaunchFilePicker(
                    request_id,
                    hwnd,
                    save,
                    _title,
                    file_name,
                    extensions,
                ) => {
                    let file_path = launch_file_picker(hwnd, save, file_name, extensions).await;

                    if let Ok(path) = file_path {
                        send_outgoing_event(JniCallback::FilePicked(request_id, path.to_string()));
                    } else {
                        send_outgoing_event(JniCallback::FilePicked(request_id, "".to_string()));
                    }
                }

                IncomingEvent::OpenUrl(url) => {
                    let url_hstring = HSTRING::from(url.clone());
                    let uri = Uri::CreateUri(&url_hstring);

                    if let Ok(uri) = uri {
                        let res = Launcher::LaunchUriAsync(&uri);

                        match res {
                            Ok(launched_operation) => match launched_operation.await {
                                Err(e) => {
                                    log::error!("Error launching URI {e:?}");
                                }
                                Ok(launched) => {
                                    if !launched {
                                        log::error!("Failed to launch URI: false");
                                    }
                                }
                            },
                            Err(e) => {
                                log::error!("Error launching URI {e:?}");
                            }
                        }
                    }
                }

                _ => {}
            }
        }

        Ok::<(), Box<dyn std::error::Error>>(())
    };

    let outgoing_events = async {
        while let Some(event) = outgoing_rx.recv().await {
            jni_callback(event);
        }

        Ok(())
    };

    // other listeners
    let ipc_commands = ipc::commands_listener(|command: String, arg: String| {
        let event = JniCallback::IpcCallback(command, arg);
        send_outgoing_event(event);
    });

    let theme_observer_future = theme_observer::observe(outgoing_tx_clone);

    tokio::try_join!(
        session_events,
        ipc_commands,
        outgoing_events,
        theme_observer_future,
        tray::tray_listener(outgoing_tx_clone2)
    )?;

    Ok(())
}

fn update_sessions(
    manager: &GlobalSystemMediaTransportControlsSessionManager,
    session_trackers: &mut HashMap<String, SessionTracker>,
    app_names_cache: &mut HashMap<String, String>,
    is_app_allowed: impl Fn(&str) -> bool,
) {
    // use a hashset to skip multiple sessions from the same app. Windows has no good way to handle those
    let mut current_app_ids: HashSet<String> = HashSet::new();

    let sessions = manager.GetSessions().unwrap();

    for session in &sessions {
        let app_id_hstring = session
            .SourceAppUserModelId()
            .expect("Failed to get app ID");

        let app_id = app_id_hstring.to_string();

        if !app_names_cache.contains_key(&app_id) {
            let app_info = AppInfo::GetFromAppUserModelId(&app_id_hstring);
            if let Ok(app_info) = app_info
                && let Ok(display_info) = app_info.DisplayInfo()
                && let Ok(display_name) = display_info.DisplayName()
            {
                let app_name = display_name.to_string();
                app_names_cache.insert(app_id.clone(), app_name);
            }
        };

        current_app_ids.insert(app_id);
    }

    // send updated session infos
    let session_infos = current_app_ids
        .iter()
        .map(|app_id| {
            let app_name = app_names_cache.get(app_id).cloned().unwrap_or_default();

            SessionInfo {
                app_id: app_id.clone(),
                app_name,
            }
        })
        .collect::<Vec<SessionInfo>>();
    let sessions_media_event = JniCallback::SessionsChanged(session_infos.clone());
    send_outgoing_event(sessions_media_event);

    let prev_app_ids = session_trackers
        .keys()
        .cloned()
        .collect::<HashSet<String>>();

    // remove cache for removed sessions
    for app_id in prev_app_ids.difference(&current_app_ids) {
        if session_trackers.remove(app_id).is_some() {
            log::debug!("Removing session tracker for removed session: {app_id}");
        }
    }

    for session in sessions {
        let app_id = session_id(&session);

        // remove listeners for removed sessions
        if !is_app_allowed(&app_id) {
            if session_trackers.remove(&app_id).is_some() {
                log::debug!("Removing session tracker for disallowed app: {app_id}");
            }
            continue;
        }

        // skip if session is already added
        if session_trackers.contains_key(&app_id) {
            continue;
        }

        let session_tracker = SessionTracker::new(session);
        session_trackers.insert(app_id.clone(), session_tracker);
    }
}

fn session_id(session: &GlobalSystemMediaTransportControlsSession) -> String {
    session
        .SourceAppUserModelId()
        .expect("Failed to get AUMID")
        .to_string()
    // let ptr = session.as_raw() as *const () as usize;
    // format!("{:X}", ptr)
}

#[derive(Debug, Default)]
struct ThrottleInfo {
    last_processed: Option<Instant>,
    event_pending: bool,
}

impl ThrottleInfo {
    fn should_throttle(&self) -> bool {
        self.last_processed
            .map(|t| t.elapsed().as_millis() < 500)
            .unwrap_or(false)
    }

    fn mark_processed(&mut self) {
        self.last_processed = Some(Instant::now());
        self.event_pending = false;
    }

    fn mark_pending(&mut self) {
        self.event_pending = true;
    }
}

#[derive(Debug)]
struct SessionTracker {
    session: GlobalSystemMediaTransportControlsSession,
    playback_info_token: i64,
    media_properties_token: i64,
    timeline_properties_token: i64,
}

impl Drop for SessionTracker {
    fn drop(&mut self) {
        log::debug!("Dropping SessionTracker {}", session_id(&self.session));

        self.session
            .RemovePlaybackInfoChanged(self.playback_info_token)
            .unwrap_or_else(|e| log::error!("Error removing PlaybackInfoChanged {e}",));
        self.session
            .RemoveMediaPropertiesChanged(self.media_properties_token)
            .unwrap_or_else(|e| log::error!("Error removing MediaPropertiesChanged {e}",));
        self.session
            .RemoveTimelinePropertiesChanged(self.timeline_properties_token)
            .unwrap_or_else(|e| log::error!("Error removing TimelinePropertiesChanged {e}",));
    }
}

impl SessionTracker {
    fn new(session: GlobalSystemMediaTransportControlsSession) -> Self {
        let playback_info_cached1 = Arc::new(Mutex::<Option<PlaybackInfo>>::new(None));
        let metadata_info_cached1 = Arc::new(Mutex::<Option<MetadataInfo>>::new(None));
        let timeline_throttle_info1 = Arc::new(Mutex::<ThrottleInfo>::new(ThrottleInfo::default()));
        let sess_id = session_id(&session);
        let id = sess_id.clone();

        let metadata_info_cached = metadata_info_cached1.clone();
        let playback_info_cached = playback_info_cached1.clone();
        let timeline_throttle_info = timeline_throttle_info1.clone();

        let playback_info_token = session
            .PlaybackInfoChanged(&TypedEventHandler::<
                GlobalSystemMediaTransportControlsSession,
                PlaybackInfoChangedEventArgs,
            >::new(move |session, _| {
                match session.as_ref() {
                    Some(session) => {
                        if let Some(playback_info) = Self::handle_playback_info_changed(session) {
                            let mut playback_info_guard = playback_info_cached.lock().unwrap();
                            *playback_info_guard = Some(playback_info.clone());

                            send_outgoing_event(JniCallback::PlaybackStateChanged(
                                id.clone(),
                                playback_info,
                            ));

                            let mut throttle_info = timeline_throttle_info.lock().unwrap();

                            if throttle_info.event_pending {
                                Self::process_timeline_event(
                                    session,
                                    &mut metadata_info_cached.lock().unwrap(),
                                    &mut playback_info_guard,
                                    &mut throttle_info,
                                );
                            }
                        }
                    }
                    None => {
                        log::error!("PlaybackInfoChanged event handler received None");
                    }
                }
                Ok(())
            }))
            .unwrap_or_else(|e| {
                log::error!("Error setting PlaybackInfoChanged event handler: {e}");
                0
            });

        let id = sess_id.clone();
        let metadata_info_cached = metadata_info_cached1.clone();
        let playback_info_cached = playback_info_cached1.clone();
        let timeline_throttle_info = timeline_throttle_info1.clone();

        let media_properties_token = session
            .MediaPropertiesChanged(&TypedEventHandler::<
                GlobalSystemMediaTransportControlsSession,
                MediaPropertiesChangedEventArgs,
            >::new(move |session, _| {
                match session.as_ref() {
                    Some(session) => {
                        if let Some(metadata_info) = Self::handle_media_properties_changed(session)
                        {
                            let mut metadata_info_guard = metadata_info_cached.lock().unwrap();
                            let duration = metadata_info_guard
                                .as_ref()
                                .map(|x| x.duration)
                                .unwrap_or(-1);

                            let metadata_info = MetadataInfo {
                                duration,
                                ..metadata_info
                            };
                            *metadata_info_guard = Some(metadata_info.clone());

                            send_outgoing_event(JniCallback::MetadataChanged(
                                id.clone(),
                                metadata_info,
                            ));

                            let mut throttle_info = timeline_throttle_info.lock().unwrap();

                            if throttle_info.event_pending {
                                Self::process_timeline_event(
                                    session,
                                    &mut metadata_info_guard,
                                    &mut playback_info_cached.lock().unwrap(),
                                    &mut throttle_info,
                                );
                            }
                        }
                    }
                    None => {
                        log::error!("MediaPropertiesChanged event handler received None");
                    }
                }
                Ok(())
            }))
            .unwrap_or_else(|e| {
                log::error!("Error setting MediaPropertiesChanged event handler: {e}");
                0
            });

        let metadata_info_cached = metadata_info_cached1.clone();
        let playback_info_cached = playback_info_cached1.clone();
        let timeline_throttle_info = timeline_throttle_info1;
        let timeline_properties_token = session
            .TimelinePropertiesChanged(&TypedEventHandler::<
                GlobalSystemMediaTransportControlsSession,
                TimelinePropertiesChangedEventArgs,
            >::new(move |session, _| {
                match session.as_ref() {
                    Some(session) => {
                        // rate limit this to prevent spam while scrubbing
                        // e.g. if 15 events come in within 500ms, only send the last one
                        // to fix "Error sending outgoing event: no available capacity"

                        let mut throttle_info = timeline_throttle_info.lock().unwrap();

                        if throttle_info.should_throttle() {
                            throttle_info.mark_pending();
                        } else {
                            Self::process_timeline_event(
                                session,
                                &mut metadata_info_cached.lock().unwrap(),
                                &mut playback_info_cached.lock().unwrap(),
                                &mut throttle_info,
                            );
                        }
                    }
                    None => {
                        log::error!("TimelinePropertiesChanged event handler received None");
                    }
                }
                Ok(())
            }))
            .unwrap_or_else(|e| {
                log::error!("Error setting TimelinePropertiesChanged event handler: {e}");
                0
            });

        // initial fetch
        let id = sess_id.clone();

        let (duration, position) =
            Self::handle_timeline_properties_changed(&session).unwrap_or((-1, -1));

        if let Some(metadata_info) = Self::handle_media_properties_changed(&session) {
            let metadata_info = MetadataInfo {
                duration,
                ..metadata_info
            };
            metadata_info_cached1
                .lock()
                .unwrap()
                .replace(metadata_info.clone());
            send_outgoing_event(JniCallback::MetadataChanged(id.clone(), metadata_info));
        }

        if let Some(playback_info) = Self::handle_playback_info_changed(&session) {
            let playback_info = PlaybackInfo {
                position,
                ..playback_info
            };
            playback_info_cached1
                .lock()
                .unwrap()
                .replace(playback_info.clone());
            send_outgoing_event(JniCallback::PlaybackStateChanged(id, playback_info));
        }

        Self {
            session,
            playback_info_token,
            media_properties_token,
            timeline_properties_token,
        }
    }

    fn handle_playback_info_changed(
        session: &GlobalSystemMediaTransportControlsSession,
    ) -> Option<PlaybackInfo> {
        if let Ok(playback_info) = session.GetPlaybackInfo()
            && let Ok(playback_status) = playback_info.PlaybackStatus()
        {
            let state = match playback_status {
                GlobalSystemMediaTransportControlsSessionPlaybackStatus::Paused => {
                    PlaybackState::Paused
                }
                GlobalSystemMediaTransportControlsSessionPlaybackStatus::Stopped => {
                    PlaybackState::Stopped
                }
                GlobalSystemMediaTransportControlsSessionPlaybackStatus::Changing
                | GlobalSystemMediaTransportControlsSessionPlaybackStatus::Opened => {
                    PlaybackState::Waiting
                }
                GlobalSystemMediaTransportControlsSessionPlaybackStatus::Closed => {
                    PlaybackState::None
                }
                GlobalSystemMediaTransportControlsSessionPlaybackStatus::Playing => {
                    PlaybackState::Playing
                }
                _ => PlaybackState::Other,
            };

            let can_skip = playback_info
                .Controls()
                .map(|c| c.IsNextEnabled())
                .unwrap_or_else(|_| Ok(false))
                .unwrap_or_default();

            // this becomes true for spotify remote
            let can_channel_down = playback_info
                .Controls()
                .map(|c| c.IsChannelDownEnabled())
                .unwrap_or_else(|_| Ok(false))
                .unwrap_or_default();

            // this doesnt, kept it anyway
            let can_channel_up = playback_info
                .Controls()
                .map(|c| c.IsChannelUpEnabled())
                .unwrap_or_else(|_| Ok(false))
                .unwrap_or_default();

            let (_, position) =
                Self::handle_timeline_properties_changed(session).unwrap_or((-1, -1));

            let playback_info = PlaybackInfo {
                state,
                can_skip,
                can_change_channel: can_channel_down || can_channel_up,
                position,
            };

            Some(playback_info)
        } else {
            log::error!("Failed to get playback state");
            None
        }
    }

    fn handle_media_properties_changed(
        session: &GlobalSystemMediaTransportControlsSession,
    ) -> Option<MetadataInfo> {
        if let Ok(media_properties_async) = session.TryGetMediaPropertiesAsync()
            && let Ok(media_properties) = media_properties_async.join()
        {
            let title = media_properties.Title().unwrap_or_default().to_string();
            let artist = media_properties.Artist().unwrap_or_default().to_string();
            let album = media_properties
                .AlbumTitle()
                .unwrap_or_default()
                .to_string();
            let album_artist = media_properties
                .AlbumArtist()
                .unwrap_or_default()
                .to_string();
            let track_number = media_properties.TrackNumber().unwrap_or_default();

            // get the existing duration if available

            let metadata_info = MetadataInfo {
                title,
                artist,
                album,
                album_artist,
                track_number,
                duration: -1, // this will be updated later from timeline properties
                art_url: String::new(), // not used on windows
                track_url: String::new(), // not used on windows
            };
            Some(metadata_info)
        } else {
            log::error!("Failed to get media properties");
            None
        }
    }

    fn handle_timeline_properties_changed(
        session: &GlobalSystemMediaTransportControlsSession,
    ) -> Option<(i64, i64)> {
        // check for LastUpdatedTime as players that don't report timeline properties return an Ok() but everything is 0
        if let Ok(timeline_properties) = session.GetTimelineProperties()
            && timeline_properties
                .LastUpdatedTime()
                .unwrap_or_default()
                .UniversalTime
                != 0
        {
            // Duration is in ns, convert to ms
            // some players dont report timeline properties, handle that with -1
            let end_time = timeline_properties
                .EndTime()
                .map(|x| x.Duration / 10000)
                .unwrap_or(-1);
            let start_time = timeline_properties
                .StartTime()
                .map(|x| x.Duration / 10000)
                .unwrap_or(-1);

            let duration = if end_time == -1 || start_time == -1 {
                -1
            } else {
                end_time - start_time
            };

            let position = timeline_properties
                .Position()
                .map(|x| x.Duration / 10000)
                .unwrap_or(-1);

            Some((duration, position))
        } else {
            None
        }
    }

    fn process_timeline_event(
        session: &GlobalSystemMediaTransportControlsSession,
        last_metadata_info: &mut Option<MetadataInfo>,
        last_playback_info: &mut Option<PlaybackInfo>,
        throttle_info: &mut ThrottleInfo,
    ) {
        let id = session_id(session);

        if let Some((duration, position)) = Self::handle_timeline_properties_changed(session) {
            // println!("position: {}, duration: {}", position, duration);

            // on windows, the duration may get updated much later than the media properties

            if duration != -1
                && let Some(last_metadata_info) = last_metadata_info
                && last_metadata_info.duration != duration
            {
                last_metadata_info.duration = duration;
                // report the updated metadata with duration
                send_outgoing_event(JniCallback::MetadataChanged(
                    id.clone(),
                    last_metadata_info.clone(),
                ));
            }

            if position != -1
                && let Some(last_playback_info) = last_playback_info
            {
                last_playback_info.position = position;

                // report the updated playback info
                send_outgoing_event(JniCallback::PlaybackStateChanged(
                    id.clone(),
                    last_playback_info.clone(),
                ));
            }
        }

        throttle_info.mark_processed();
    }
}

fn show_notification(title: &str, body: &str) -> windows::core::Result<()> {
    const AUMID: &str = "com.arn.scrobble";

    let template = r#"
        <toast activationType="protocol" launch="">
            <visual>
                <binding template="ToastGeneric">
                    <text></text>
                    <text></text>
                </binding>
            </visual>
        </toast>
    "#;
    let toast_xml = XmlDocument::new()?;
    toast_xml.LoadXml(&HSTRING::from(template))?;
    let text_nodes = toast_xml.GetElementsByTagName(&HSTRING::from("text"))?;
    text_nodes
        .Item(0)?
        .AppendChild(&toast_xml.CreateTextNode(&HSTRING::from(title))?)?;
    text_nodes
        .Item(1)?
        .AppendChild(&toast_xml.CreateTextNode(&HSTRING::from(body))?)?;

    // put self exe path in the protocol arguments
    let exe_path = std::env::current_exe()
        .ok()
        .and_then(|p| p.to_str().map(|s| s.to_string()))
        .unwrap_or_default();

    toast_xml
        .DocumentElement()?
        .SetAttribute(&HSTRING::from("launch"), &HSTRING::from(exe_path))?;

    let toast = ToastNotification::CreateToastNotification(&toast_xml)?;
    toast.SetExpiresOnReboot(true)?;

    let notifier = ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(AUMID))?;
    notifier.Show(&toast)?;

    Ok(())
}
