#[cfg(target_os = "windows")]
mod windows_utils;

#[cfg(target_os = "linux")]
mod linux_utils;

#[cfg(target_os = "windows")]
pub use windows_utils::*;

#[cfg(target_os = "linux")]
pub use linux_utils::apply_blur_to_window;
