//! macOS media listener.
//!
//! Now-playing comes from the `ungive/mediaremote-adapter` Perl bridge (validated
//! in the Phase 0 spike — no longer in the tree): `/usr/bin/perl` is Apple-signed
//! (`com.apple.perl`) and may load the private `MediaRemote.framework` even on
//! macOS 15.4+, where a normal third-party read returns nil. We spawn the bridge
//! in `stream` mode and translate its newline-delimited JSON into the same
//! `SessionsChanged` / `MetadataChanged` / `PlaybackStateChanged` callbacks the
//! Windows/Linux listeners emit.
//!
//! Coverage note (see MACOS.md §6.6): macOS exposes a single aggregated Now
//! Playing item, and only for apps that publish to `MPNowPlayingInfoCenter`.
//! Apps that don't (some Electron clients) are invisible here, by design for v1.
//!
//! Adapter location is resolved in order:
//!   1. Env vars (dev / harness) — the only path when running via `./gradlew
//!      :composeApp:run` (raw JVM, no `.app` bundle):\n//!        PANO_MR_PERL       (default /usr/bin/perl)
//!        PANO_MR_SCRIPT     path to mediaremote-adapter.pl
//!        PANO_MR_FRAMEWORK  path to MediaRemoteAdapter.framework
//!   2. Bundled inside the packaged `.app` at
//!      `Contents/app/resources/mediaremote-adapter/` (jpackage layout; Phase 4
//!      distribution path; no env vars needed once the DMG bundles the adapter).
//! If neither is available, the listener still runs (IPC, notifications, theme,
//! open-URL) but emits no now-playing — so the app degrades gracefully.
//!
//! Phase 3b (playback control + artwork):
//!   - `skip` via `perl … send 4` (kMRNextTrack); `can_skip` is reported when the
//!     adapter is up and the item isn't flagged `prohibitsSkip` (e.g. ads).
//!   - `artworkData` (base64) is decoded to a temp file and surfaced as a
//!     `file://` `art_url` on `MetadataChanged`.
//!   - the app display name is resolved from the bundle id via `mdfind` + the
//!     app's `Info.plist` (cache, falls back to the bundle-id tail).
//!   - `mute`/`unmute` degrade to a no-op: macOS has no public per-app mute API.

use std::collections::HashMap;
use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;

use base64::Engine;
use serde::Deserialize;
use tokio::sync::mpsc;

use crate::INCOMING_PLAYER_EVENT_TX;
use crate::file_picker::{launch_file_picker, posix_path_to_file_uri};
use crate::jni_callback::JniCallback;
use crate::media_events::{IncomingEvent, MetadataInfo, PlaybackInfo, PlaybackState, SessionInfo};
use crate::{ipc, theme_observer};

static OUTGOING_PLAYER_EVENT_TX: OnceLock<mpsc::Sender<JniCallback>> = OnceLock::new();

fn send_outgoing_event(event: JniCallback) {
    if let Some(tx) = OUTGOING_PLAYER_EVENT_TX.get() {
        if let Err(e) = tx.try_send(event) {
            log::error!("Error sending outgoing event: {e}");
        }
    } else {
        log::error!("Outgoing sender not initialized");
    }
}

// ── adapter wire format ──────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct StreamEnvelope {
    #[serde(rename = "type")]
    kind: String,
    payload: Option<Payload>,
    #[serde(default)]
    diff: bool,
}

#[derive(Debug, Deserialize, Default, Clone)]
#[serde(rename_all = "camelCase")]
struct Payload {
    bundle_identifier: Option<String>,
    parent_application_bundle_identifier: Option<String>,
    playing: Option<bool>,
    title: Option<String>,
    artist: Option<String>,
    album: Option<String>,
    duration: Option<f64>,     // seconds
    elapsed_time: Option<f64>, // seconds
    // Phase 3b additions (all optional; the adapter omits them when unavailable).
    prohibits_skip: Option<bool>,
    artwork_data: Option<String>,       // base64-encoded image bytes
    artwork_mime_type: Option<String>, // e.g. "image/png"
}

impl Payload {
    /// Overlay non-null fields from `next` (the adapter sends partial `diff` updates).
    fn merge(&mut self, next: &Payload) {
        macro_rules! take {
            ($f:ident) => {
                if next.$f.is_some() {
                    self.$f = next.$f.clone();
                }
            };
        }
        take!(bundle_identifier);
        take!(parent_application_bundle_identifier);
        take!(playing);
        take!(title);
        take!(artist);
        take!(album);
        take!(duration);
        take!(elapsed_time);
        take!(prohibits_skip);
        take!(artwork_data);
        take!(artwork_mime_type);
    }
}

/// Normalized now-playing snapshot handed from the reader thread to the runtime.
#[derive(Debug, Clone)]
struct NowPlaying {
    app_id: String,
    app_name: String,
    title: String,
    artist: String,
    album: String,
    duration_ms: i64,
    position_ms: i64,
    playing: bool,
    artwork_b64: Option<String>,
    artwork_mime: Option<String>,
    prohibits_skip: bool,
}

fn to_now_playing(p: &Payload) -> Option<NowPlaying> {
    let app_id = p
        .parent_application_bundle_identifier
        .clone()
        .or_else(|| p.bundle_identifier.clone())
        .filter(|s| !s.is_empty())?;

    // Phase 3a: best-effort name from the bundle id; NSRunningApplication later.
    let app_name = app_id.rsplit('.').next().unwrap_or(&app_id).to_string();

    Some(NowPlaying {
        app_id,
        app_name,
        title: p.title.clone().unwrap_or_default(),
        artist: p.artist.clone().unwrap_or_default(),
        album: p.album.clone().unwrap_or_default(),
        duration_ms: p.duration.map(|d| (d * 1000.0) as i64).unwrap_or(-1),
        position_ms: p.elapsed_time.map(|d| (d * 1000.0) as i64).unwrap_or(-1),
        playing: p.playing.unwrap_or(false),
        artwork_b64: p.artwork_data.clone(),
        artwork_mime: p.artwork_mime_type.clone(),
        prohibits_skip: p.prohibits_skip.unwrap_or(false),
    })
}

// ── adapter process ──────────────────────────────────────────────────────────

/// Resolve the MediaRemote adapter to (perl, script, framework). Tries, in order:
///   1. `PANO_MR_*` env vars — the dev/harness path (and the only path when
///      running via `./gradlew :composeApp:run`, since that's a raw JVM, not a
///      `.app` bundle).
///   2. The adapter bundled inside the packaged `.app` at
///      `Contents/app/resources/mediaremote-adapter/` (jpackage layout;
///      derived from `current_exe`). This is the distribution path: once the
///      adapter is bundled (Phase 4), now-playing works with no env vars.
/// Returns `None` (and the listener degrades to no now-playing) if neither is
/// available.
fn resolve_adapter() -> Option<(String, String, String)> {
    if let Some(t) = resolve_adapter_from_env() {
        return Some(t);
    }
    if let Some(t) = resolve_adapter_from_bundle() {
        return Some(t);
    }
    log::warn!(
        "MediaRemote adapter not found. Set PANO_MR_SCRIPT/PANO_MR_FRAMEWORK for dev,\n         or build + package via the sibling repo's copy-builds-to-pano-dir.sh and\n         packageReleaseDmg for distribution. Now-playing is disabled;\n         IPC/notifications/theme/open-URL remain active."
    );
    None
}

fn resolve_adapter_from_env() -> Option<(String, String, String)> {
    let perl = std::env::var("PANO_MR_PERL").unwrap_or_else(|_| "/usr/bin/perl".to_string());
    let script = std::env::var("PANO_MR_SCRIPT").ok()?;
    let framework = std::env::var("PANO_MR_FRAMEWORK").ok()?;
    if !Path::new(&script).exists() || !Path::new(&framework).exists() {
        log::debug!("PANO_MR_* set but script/framework missing on disk; trying bundle");
        return None;
    }
    Some((perl, script, framework))
}

fn resolve_adapter_from_bundle() -> Option<(String, String, String)> {
    let app = bundled_app_path()?;
    // jpackage places `appResourcesRootDir` under `Contents/app/resources/`
    // (this is `$APPDIR/resources` in the `-Dpano.native.components.path` JVM
    // arg). Try that first; fall back to the macOS-conventional
    // `Contents/Resources/` in case a different bundling tool is ever used.
    let candidates = [
        PathBuf::from(&app).join("Contents/app/resources/mediaremote-adapter"),
        PathBuf::from(&app).join("Contents/Resources/mediaremote-adapter"),
    ];
    for res in &candidates {
        let script = res.join("mediaremote-adapter.pl");
        let framework = res.join("MediaRemoteAdapter.framework");
        if script.exists() && framework.exists() {
            return Some((
                "/usr/bin/perl".to_string(),
                script.to_string_lossy().into_owned(),
                framework.to_string_lossy().into_owned(),
            ));
        }
    }
    None
}

/// Spawn the Perl adapter in `stream` mode. A dedicated thread reads its stdout,
/// merges diff updates, and forwards normalized snapshots over `updates_tx`
/// (`None` = playback cleared). The child is stored in `child_handle` so the
/// Shutdown handler can kill it. Returns false if the adapter is unavailable.
fn start_adapter(
    child_handle: Arc<Mutex<Option<Child>>>,
    updates_tx: mpsc::Sender<Option<NowPlaying>>,
) -> bool {
    let Some((perl, script, framework)) = resolve_adapter() else {
        return false;
    };

    let mut child = match Command::new(&perl)
        .arg(&script)
        .arg(&framework)
        .arg("stream")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            log::error!("Failed to spawn MediaRemote adapter: {e}");
            return false;
        }
    };

    let Some(stdout) = child.stdout.take() else {
        log::error!("MediaRemote adapter produced no stdout");
        return false;
    };
    *child_handle.lock().unwrap() = Some(child);

    thread::Builder::new()
        .name("MediaRemoteAdapter".to_string())
        .spawn(move || {
            let reader = std::io::BufReader::new(stdout);
            let mut state = Payload::default();

            for line in reader.lines() {
                let line = match line {
                    Ok(l) => l,
                    Err(e) => {
                        log::error!("adapter read error: {e}");
                        break;
                    }
                };
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }

                let envelope: StreamEnvelope = match serde_json::from_str(line) {
                    Ok(e) => e,
                    Err(e) => {
                        log::debug!("adapter parse error: {e}");
                        continue;
                    }
                };
                if envelope.kind != "data" {
                    continue;
                }

                let update = match envelope.payload {
                    Some(p) => {
                        if envelope.diff {
                            state.merge(&p);
                        } else {
                            state = p;
                        }
                        to_now_playing(&state)
                    }
                    None => {
                        state = Payload::default();
                        None
                    }
                };

                if updates_tx.blocking_send(update).is_err() {
                    break; // runtime gone
                }
            }
            log::info!("MediaRemote adapter reader thread finished");
        })
        .expect("failed to spawn adapter reader thread");

    true
}

// ── listener ─────────────────────────────────────────────────────────────────

#[tokio::main(flavor = "current_thread")]
pub async fn listener(
    jni_callback: impl Fn(JniCallback) -> Option<bool> + 'static,
) -> Result<(), Box<dyn std::error::Error>> {
    let (incoming_tx, mut incoming_rx) = mpsc::channel(10);
    *INCOMING_PLAYER_EVENT_TX.lock().unwrap() = Some(incoming_tx);

    let (outgoing_tx, mut outgoing_rx) = mpsc::channel(100);
    let outgoing_tx_clone = outgoing_tx.clone();
    OUTGOING_PLAYER_EVENT_TX.set(outgoing_tx).unwrap();

    let child_handle: Arc<Mutex<Option<Child>>> = Arc::new(Mutex::new(None));

    let (updates_tx, mut updates_rx) = mpsc::channel::<Option<NowPlaying>>(32);
    let adapter_running = start_adapter(child_handle.clone(), updates_tx);
    if adapter_running {
        log::info!("MediaRemote adapter started");
    } else {
        log::warn!(
            "macOS now-playing source unavailable (set PANO_MR_SCRIPT / PANO_MR_FRAMEWORK). \
             IPC, notifications, theme and open-URL remain active."
        );
    }

    let is_app_allowed = |app_id: &str| -> bool {
        jni_callback(JniCallback::IsAppIdAllowed(app_id.to_string())).unwrap_or(false)
    };

    // Translate adapter snapshots into Java callbacks. Only emits on actual change
    // to avoid spamming the scrobble pipeline.
    let media_events = async {
        let mut last_app_id: Option<String> = None;
        let mut last_meta: Option<(String, String, String, i64, String)> = None;
        let mut last_pb: Option<(bool, i64, bool)> = None;
        // Artwork is decoded once per distinct payload and reused until it changes;
        // the adapter itself re-uses artwork across same-track diffs (see its README),
        // so this stays cheap. `current_art_url` persists across updates so a brief
        // `artworkData: null` in a diff does not blank out the cover.
        let mut current_art_url = String::new();
        let mut last_artwork_b64: Option<String> = None;

        while let Some(update) = updates_rx.recv().await {
            match update {
                None => {
                    if last_app_id.take().is_some() {
                        current_art_url.clear();
                        last_artwork_b64 = None;
                        send_outgoing_event(JniCallback::SessionsChanged(vec![]));
                        last_meta = None;
                        last_pb = None;
                    }
                }
                Some(np) => {
                    if last_app_id.as_deref() != Some(np.app_id.as_str()) {
                        last_app_id = Some(np.app_id.clone());
                        last_meta = None;
                        last_pb = None;
                        current_art_url.clear();
                        last_artwork_b64 = None;
                        // Resolve the human-readable app name once per session switch
                        // (Spotlight + Info.plist; falls back to the bundle-id tail).
                        let resolved_name = resolve_app_name(&np.app_id);
                        send_outgoing_event(JniCallback::SessionsChanged(vec![SessionInfo {
                            app_id: np.app_id.clone(),
                            app_name: resolved_name,
                        }]));
                    }

                    if !is_app_allowed(&np.app_id) {
                        continue;
                    }

                    // Decode artwork only when the base64 payload actually changed.
                    if np.artwork_b64.as_deref() != last_artwork_b64.as_deref() {
                        last_artwork_b64 = np.artwork_b64.clone();
                        if let Some(b64) = np.artwork_b64.as_ref().filter(|s| !s.is_empty()) {
                            let mime = np.artwork_mime.as_deref().unwrap_or("");
                            if let Some(uri) =
                                decode_artwork_to_uri(&np.app_id, b64, mime)
                            {
                                current_art_url = uri;
                            }
                        }
                    }

                    let meta = (
                        np.title.clone(),
                        np.artist.clone(),
                        np.album.clone(),
                        np.duration_ms,
                        current_art_url.clone(),
                    );
                    if last_meta.as_ref() != Some(&meta) {
                        last_meta = Some(meta);
                        send_outgoing_event(JniCallback::MetadataChanged(
                            np.app_id.clone(),
                            MetadataInfo {
                                title: np.title.clone(),
                                artist: np.artist.clone(),
                                album: np.album.clone(),
                                album_artist: String::new(),
                                track_number: 0,
                                duration: np.duration_ms,
                                art_url: current_art_url.clone(),
                                track_url: String::new(),
                            },
                        ));
                    }

                    // Skip is only actionable when the adapter is available (we send
                    // via it) and the player doesn't flag the item as unskippable
                    // (e.g. ads set `prohibitsSkip`).
                    let can_skip = adapter_running && !np.prohibits_skip;
                    let pb = (np.playing, np.position_ms, can_skip);
                    if last_pb != Some(pb) {
                        last_pb = Some(pb);
                        send_outgoing_event(JniCallback::PlaybackStateChanged(
                            np.app_id.clone(),
                            PlaybackInfo {
                                state: if np.playing {
                                    PlaybackState::Playing
                                } else {
                                    PlaybackState::Paused
                                },
                                position: np.position_ms,
                                can_skip,
                            },
                        ));
                    }
                }
            }
        }

        Ok::<(), Box<dyn std::error::Error>>(())
    };

    let incoming_events = async {
        while let Some(event) = incoming_rx.recv().await {
            match event {
                IncomingEvent::Shutdown => {
                    INCOMING_PLAYER_EVENT_TX.lock().unwrap().take();
                    if let Some(mut child) = child_handle.lock().unwrap().take() {
                        let _ = child.kill();
                    }
                    break;
                }

                IncomingEvent::Notification(title, body) => {
                    show_notification(&title, &body);
                }

                IncomingEvent::OpenUrl(url) => {
                    open_url(&url);
                }

                IncomingEvent::LaunchFilePicker(request_id, _hwnd, save, title, name, ext) => {
                    // NSOpenPanel / NSSavePanel via `osascript` (see file_picker.rs).
                    let uri = launch_file_picker(save, title, name, ext).await;
                    send_outgoing_event(JniCallback::FilePicked(request_id, uri));
                }

                IncomingEvent::Skip(_) => {
                    // macOS exposes a single aggregated Now Playing session, so the
                    // app_id in the event is informational; the command always targets
                    // the current now-playing app. kMRNextTrack = 4 (see the adapter
                    // README "send COMMAND"). Run off the async runtime: a blocking
                    // `.status()` here would stall the stream/outgoing pumps.
                    let adapter_up = resolve_adapter().is_some();
                    if adapter_up {
                        let _ = tokio::task::spawn_blocking(|| {
                            send_media_command("4");
                        })
                        .await;
                    } else {
                        log::debug!(
                            "skip requested but MediaRemote adapter is unavailable"
                        );
                    }
                }

                IncomingEvent::Mute(_) | IncomingEvent::Unmute(_) => {
                    // macOS has no public per-app mute API (only system volume via
                    // CoreAudio). Degrade to a no-op for v1 (MACOS.md §6.4 / §13).
                    log::debug!("mute/unmute is a no-op on macOS (no per-app mute API)");
                }

                IncomingEvent::AutoStart(add) => {
                    set_autostart(add);
                }

                IncomingEvent::RefreshSessions => {
                    // The adapter streams continuously; nothing to re-poll.
                }
            }
        }

        Ok::<(), Box<dyn std::error::Error>>(())
    };

    let outgoing_events = async {
        while let Some(event) = outgoing_rx.recv().await {
            jni_callback(event);
        }

        Ok::<(), Box<dyn std::error::Error>>(())
    };

    let ipc_commands = ipc::commands_listener(|command: String, arg: String| {
        send_outgoing_event(JniCallback::IpcCallback(command, arg));
    });

    let theme_observer_future = theme_observer::observe(outgoing_tx_clone);

    tokio::try_join!(
        media_events,
        incoming_events,
        ipc_commands,
        outgoing_events,
        theme_observer_future
    )?;

    Ok(())
}

fn show_notification(title: &str, body: &str) {
    let script = format!(
        "display notification {} with title {}",
        applescript_quote(body),
        applescript_quote(title)
    );
    if let Err(e) = Command::new("/usr/bin/osascript")
        .arg("-e")
        .arg(&script)
        .status()
    {
        log::error!("osascript notification failed: {e}");
    }
}

fn open_url(url: &str) {
    if let Err(e) = Command::new("/usr/bin/open").arg(url).status() {
        log::error!("open url failed: {e}");
    }
}

/// Quote a string as an AppleScript string literal.
fn applescript_quote(s: &str) -> String {
    let escaped = s.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

// ── Phase 3b: playback control, artwork, app name ───────────────────────────

/// Send a MediaRemote command to the now-playing app via a one-shot adapter
/// invocation: `perl script framework send <id>`. Each command is a fresh,
/// short-lived process (the `stream` child keeps running independently). Command
/// IDs: 0=play, 1=pause, 2=toggle, 4=next, 5=prev, … (see adapter README).
fn send_media_command(cmd: &str) {
    let Some((perl, script, framework)) = resolve_adapter() else {
        log::debug!("MediaRemote send {cmd} skipped: adapter unavailable");
        return;
    };
    match Command::new(&perl)
        .arg(&script)
        .arg(&framework)
        .arg("send")
        .arg(cmd)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
    {
        Ok(s) if s.success() => log::debug!("MediaRemote send {cmd} ok"),
        Ok(s) => log::warn!("MediaRemote send {cmd} reported failure: {s}"),
        Err(e) => log::error!("MediaRemote send {cmd} spawn error: {e}"),
    }
}

/// Map an artwork MIME type to a file extension (defaults to `img` for unknowns;
/// Coil on desktop sniffs content, so the extension is only cosmetic).
fn artwork_extension(mime: &str) -> &'static str {
    if mime.contains("png") {
        "png"
    } else if mime.contains("jpeg") || mime.contains("jpg") {
        "jpg"
    } else if mime.contains("gif") {
        "gif"
    } else if mime.contains("webp") {
        "webp"
    } else {
        "img"
    }
}

/// Decode base64 artwork into a temp file and return a `file://` URI. The file is
/// written to a tmp file then atomically renamed so a concurrent image-loader
/// read never sees a half-written file. Returns `None` on decode/IO failure (the
/// caller keeps the previous artwork in that case). Files live under the system
/// temp dir and are reused across runs; the OS reaps temp dirs, so no explicit
/// cleanup is wired for v1.
fn decode_artwork_to_uri(app_id: &str, b64: &str, mime: &str) -> Option<String> {
    decode_artwork_to_uri_in(&std::env::temp_dir().join("pano-scrobbler-artwork"), app_id, b64, mime)
}

/// Same as [`decode_artwork_to_uri`] but into a caller-chosen directory (used by
/// tests so they don't litter the real temp dir, and avoid mutating `TMPDIR`).
fn decode_artwork_to_uri_in(dir: &Path, app_id: &str, b64: &str, mime: &str) -> Option<String> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(b64.as_bytes())
        .ok()?;
    if bytes.is_empty() {
        return None;
    }

    if let Err(e) = std::fs::create_dir_all(dir) {
        log::warn!("artwork: could not create temp dir {}: {e}", dir.display());
        return None;
    }

    let safe = app_id
        .replace(|c: char| !c.is_alphanumeric() && c != '.' && c != '-' && c != '_', "_");
    let ext = artwork_extension(mime);
    let final_path = dir.join(format!("{safe}.{ext}"));
    let tmp_path = dir.join(format!("{safe}.{ext}.tmp"));

    if let Err(e) = std::fs::write(&tmp_path, &bytes) {
        log::warn!("artwork: write failed: {e}");
        return None;
    }
    // Rename is atomic when src and dst are on the same volume (they are — both
    // under $TMPDIR). This avoids a torn read by the Coil image loader.
    if let Err(e) = std::fs::rename(&tmp_path, &final_path) {
        log::warn!("artwork: rename failed: {e}");
        let _ = std::fs::remove_file(&tmp_path);
        return None;
    }

    Some(posix_path_to_file_uri(&final_path.to_string_lossy()))
}

/// Best-effort bundle-id tail, used when the real app name can't be resolved.
/// `com.github.th-ch.youtube-music` → `youtube-music`, `com.apple.Music` → `Music`.
fn fallback_app_name(bundle_id: &str) -> String {
    bundle_id.rsplit('.').next().unwrap_or(bundle_id).to_string()
}

/// Resolve the human-readable app name for a bundle id by locating the `.app` via
/// Spotlight (`mdfind`, no TCC prompt) and reading `CFBundleName` from its
/// `Info.plist` (`defaults read`). Cached per bundle id. Returns the fallback
/// (bundle-id tail) when the app isn't indexed or anything fails. This mirrors
/// the port's shell-out style — no `objc2`/`NSRunningApplication` dependency for v1.
fn resolve_app_name(bundle_id: &str) -> String {
    static CACHE: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(name) = cache.lock().unwrap().get(bundle_id) {
        return name.clone();
    }

    let resolved = try_resolve_app_name(bundle_id);
    let name = resolved.unwrap_or_else(|| fallback_app_name(bundle_id));
    cache
        .lock()
        .unwrap()
        .insert(bundle_id.to_string(), name.clone());
    name
}

fn try_resolve_app_name(bundle_id: &str) -> Option<String> {
    // Spotlight lookup of the app bundle path by its CFBundleIdentifier.
    let query = format!("kMDItemCFBundleIdentifier == '{bid}'", bid = bundle_id);
    let output = Command::new("/usr/bin/mdfind")
        .arg(&query)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let app_path = String::from_utf8_lossy(&output.stdout)
        .lines()
        .find(|l| l.ends_with(".app"))?
        .trim()
        .to_string();
    if app_path.is_empty() {
        return None;
    }

    // Read CFBundleName from the app's Info.plist.
    let info_plist = format!("{app_path}/Contents/Info.plist");
    let output = Command::new("/usr/bin/defaults")
        .arg("read")
        .arg(&info_plist)
        .arg("CFBundleName")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let name = String::from_utf8_lossy(&output.stdout).trim().to_string();
    // `defaults read` wraps the value in double quotes; strip one layer if present.
    let name = name
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or(&name)
        .to_string();
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}

// ── Phase 2: login-item autostart via LaunchAgent ────────────────────────────
//
// "Run at login" on macOS launches the `.app` itself when the user logs in. We
// implement it with a per-user LaunchAgent plist in ~/Library/LaunchAgents/:
//   * presence of the file at login → launchd runs `open <app> --args --minimized`
//     (starts the app hidden in the menu bar, matching Linux's `--minimized`).
//   * removal of the file → disabled.
//
// This mirrors the rest of the macOS port's shell-out / file-IO style (no `objc2`,
// no `SMAppService` binding) and needs NO TCC permission prompt (unlike the
// AppleScript "System Events" login-item trick). It only works for a packaged
// `.app` bundle; in dev (raw JVM via gradlew) it logs a warning and no-ops —
// matching Linux, which also needs an installed binary/AppImage.
//
// The plist takes effect at the *next* login; we don't `launchctl load` it live
// (that API is deprecated on recent macOS and isn't needed for RunAtLoad).

/// Bundle id of the app (matches `APP_ID` in build.gradle.kts). Used as the
/// LaunchAgent label and as a stable, unique filename.
const BUNDLE_ID: &str = "com.arn.scrobble";

fn set_autostart(add: bool) {
    let Some(app_path) = bundled_app_path() else {
        log::warn!(
            "autostart ignored: not running from a .app bundle (dev mode). Build a DMG with \
             packageReleaseDmg to enable login-item autostart."
        );
        return;
    };

    let Some(plist_path) = launch_agent_plist_path() else {
        return; // already logged
    };

    if !add {
        if plist_path.exists()
            && let Err(e) = std::fs::remove_file(&plist_path)
        {
            log::error!("autostart: remove plist failed: {e}");
            return;
        }
        log::info!("autostart disabled (removed {})", plist_path.display());
        return;
    }

    let label = launch_agent_label();
    let plist = autostart_plist_xml(&label, &app_path);

    if let Some(parent) = plist_path.parent()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        log::error!("autostart: create LaunchAgents dir failed: {e}");
        return;
    }
    if let Err(e) = std::fs::write(&plist_path, plist) {
        log::error!("autostart: write plist failed: {e}");
        return;
    }
    log::info!(
        "autostart enabled — will launch at next login ({}). App path: {}",
        plist_path.display(),
        app_path
    );
}

fn launch_agent_label() -> String {
    format!("{BUNDLE_ID}.autostart")
}

fn launch_agent_plist_path() -> Option<PathBuf> {
    let home = std::env::var("HOME").ok().filter(|s| !s.is_empty())?;
    Some(
        PathBuf::from(home)
            .join("Library/LaunchAgents")
            .join(format!("{}.plist", launch_agent_label())),
    )
}

/// Build the LaunchAgent plist. `open <app> --args --minimized` launches the
/// bundled app and forwards `--minimized` to main(), starting it in the tray.
fn autostart_plist_xml(label: &str, app_path: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n\
         <dict>\n\
         \t<key>Label</key>\n\
         \t<string>{label}</string>\n\
         \t<key>ProgramArguments</key>\n\
         \t<array>\n\
         \t\t<string>/usr/bin/open</string>\n\
         \t\t<string>{app}</string>\n\
         \t\t<string>--args</string>\n\
         \t\t<string>--minimized</string>\n\
         \t</array>\n\
         \t<key>RunAtLoad</key>\n\
         \t<true/>\n\
         \t<key>LaunchOnlyOnce</key>\n\
         \t<true/>\n\
         </dict>\n\
         </plist>\n",
        label = label,
        app = app_path
    )
}

/// If the current executable lives inside a `.app` bundle, return the bundle
/// path (e.g. `/Applications/Pano Scrobbler.app`). Returns `None` in dev (the
/// raw JVM) or if the path can't be determined.
fn bundled_app_path() -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    let mut path: PathBuf = exe.clone();
    for _ in 0..16 {
        if path.extension().is_some_and(|ext| ext == "app") {
            return Some(path.to_string_lossy().into_owned());
        }
        if !path.pop() {
            break;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn artwork_extension_maps_common_mimes() {
        assert_eq!(artwork_extension("image/png"), "png");
        assert_eq!(artwork_extension("image/jpeg"), "jpg");
        assert_eq!(artwork_extension("image/gif"), "gif");
        assert_eq!(artwork_extension("image/webp"), "webp");
        assert_eq!(artwork_extension("application/octet-stream"), "img");
    }

    #[test]
    fn fallback_app_name_takes_bundle_tail() {
        assert_eq!(fallback_app_name("com.apple.Music"), "Music");
        assert_eq!(
            fallback_app_name("com.github.th-ch.youtube-music"),
            "youtube-music"
        );
    }

    #[test]
    fn decode_artwork_writes_file_and_returns_file_uri() {
        // 1x1 transparent PNG.
        const PNG_B64: &str =
            "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==";
        // Use a throwaway sandbox dir (no global TMPDIR mutation — that is unsafe
        // under the 2024 edition and racy across tests).
        let sandbox = std::env::temp_dir().join(format!(
            "pano-artwork-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&sandbox);

        let uri = decode_artwork_to_uri_in(&sandbox, "com.test.app", PNG_B64, "image/png").unwrap();

        assert!(
            uri.starts_with("file://"),
            "expected a file:// URI, got {uri}"
        );
        assert!(uri.contains("com.test.app.png"));
        // The PNG signature should be present in the written file.
        let written = sandbox.join("com.test.app.png");
        let bytes = std::fs::read(&written).unwrap();
        assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
        let _ = std::fs::remove_dir_all(&sandbox);
    }

    #[test]
    fn decode_artwork_rejects_garbage_base64() {
        let sandbox = std::env::temp_dir().join(format!(
            "pano-artwork-test-garbage-{}",
            std::process::id()
        ));
        assert!(decode_artwork_to_uri_in(&sandbox, "com.test.app", "!!!not base64!!!", "image/png").is_none());
        let _ = std::fs::remove_dir_all(&sandbox);
    }

    #[test]
    fn launch_agent_label_is_namespaced() {
        assert_eq!(launch_agent_label(), "com.arn.scrobble.autostart");
    }

    #[test]
    fn autostart_plist_contains_app_path_minimized_and_runatload() {
        let xml = autostart_plist_xml("com.arn.scrobble.autostart", "/Apps/Pano Scrobbler.app");
        assert!(xml.contains("<string>com.arn.scrobble.autostart</string>"));
        assert!(xml.contains("<string>/Apps/Pano Scrobbler.app</string>"));
        assert!(xml.contains("<string>--minimized</string>"));
        assert!(xml.contains("<key>RunAtLoad</key>"));
        assert!(xml.contains("<true/>"));
        assert!(xml.contains("/usr/bin/open"));
    }

    #[test]
    fn bundled_app_path_returns_none_in_dev() {
        // Running under `cargo test` the current_exe is the test binary, not a
        // `.app` bundle — so this should be None.
        assert_eq!(bundled_app_path(), None);
    }
}
