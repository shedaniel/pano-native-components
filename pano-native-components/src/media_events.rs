use strum::EnumString;

#[derive(Debug, Clone)]
pub struct MetadataInfo {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub album_artist: String,
    pub track_number: i32,
    pub duration: i64,
    pub art_url: String,
    pub track_url: String,
}

#[derive(Debug, Clone)]
pub struct PlaybackInfo {
    pub state: PlaybackState,
    pub position: i64,
    pub can_skip: bool,
    pub can_change_channel: bool,
}

#[derive(Debug, Clone)]
pub struct TimelineInfo {
    pub duration: i64,
    pub position: i64,
    pub last_updated: i64,
}

#[derive(Debug, Clone)]
pub struct SessionInfo {
    pub app_id: String,
    pub app_name: String,
}

#[derive(EnumString, strum::Display, Debug, Clone, PartialEq, Eq, Hash)]
pub enum PlaybackState {
    None,
    Stopped,
    Paused,
    Playing,
    Waiting,
    Other,
}

#[derive(Debug, Clone)]
pub enum IncomingEvent {
    Skip(String),
    Mute(String),
    Unmute(String),
    RefreshSessions,
    Shutdown,
    #[cfg(target_os = "linux")]
    OpenUrl(String),
    LaunchFilePicker(i32, i64, bool, String, String, Vec<String>),
    #[cfg(target_os = "linux")]
    AutoStart(bool),
    Notification(String, String),
}
