pub const DEFAULT_IDLE_TIMEOUT_MS: u32 = 60_000;

#[derive(PartialEq, Clone)]
pub struct Media {
    pub title: String,
    pub artist: String,
}

pub trait OSFeatures {
    fn new() -> Self
    where
        Self: Sized;

    fn supports_media(&self) -> bool;
    fn get_media(&mut self) -> Option<Media>;

    fn supports_idle(&self) -> bool;
    fn is_idle(&mut self) -> bool;
    /// Change how long without keyboard/mouse input counts as idle.
    fn set_idle_timeout(&mut self, ms: u32);
    /// Whether the session is locked (lock screen showing).
    fn is_locked(&mut self) -> bool {
        false
    }
}

#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "windows")]
pub use windows::OSImpl;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::OSImpl;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::OSImpl;
