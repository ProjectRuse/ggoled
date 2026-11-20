use super::{DEFAULT_IDLE_TIMEOUT_MS, Media, OSFeatures};
use std::mem::size_of;
use windows::Media::Control::{
    GlobalSystemMediaTransportControlsSession, GlobalSystemMediaTransportControlsSessionManager,
    GlobalSystemMediaTransportControlsSessionPlaybackStatus,
};
use windows_sys::Win32::{
    System::SystemInformation::GetTickCount,
    UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO},
};

pub struct OSImpl {
    mgr: Option<GlobalSystemMediaTransportControlsSessionManager>,
    idle_timeout_ms: u32,
}

fn is_playing(session: &GlobalSystemMediaTransportControlsSession) -> bool {
    session
        .GetPlaybackInfo()
        .and_then(|info| info.PlaybackStatus())
        .is_ok_and(|status| status == GlobalSystemMediaTransportControlsSessionPlaybackStatus::Playing)
}
impl OSFeatures for OSImpl {
    fn new() -> Self {
        let mgr = GlobalSystemMediaTransportControlsSessionManager::RequestAsync()
            .map(|req| req.join().ok())
            .ok()
            .flatten();
        Self {
            mgr,
            idle_timeout_ms: DEFAULT_IDLE_TIMEOUT_MS,
        }
    }

    fn supports_media(&self) -> bool {
        self.mgr.is_some()
    }
    fn get_media(&mut self) -> Option<Media> {
        let mgr = self.mgr.as_ref()?;
        // Windows' "current" session is often a browser tab or Discord rather than
        // the app actually playing (e.g. Spotify). Prefer the current session if it's
        // playing, otherwise take the first session that is.
        let mut candidates: Vec<GlobalSystemMediaTransportControlsSession> = vec![];
        if let Ok(current) = mgr.GetCurrentSession() {
            candidates.push(current);
        }
        if let Ok(all) = mgr.GetSessions() {
            candidates.extend(all);
        }
        let session = candidates.into_iter().find(is_playing)?;
        (|| {
            let media = session.TryGetMediaPropertiesAsync()?.join()?;
            anyhow::Ok(Media {
                title: media.Title()?.to_string_lossy(),
                artist: media.Artist()?.to_string_lossy(),
            })
        })()
        .ok()
    }

    fn supports_idle(&self) -> bool {
        true
    }
    fn is_idle(&mut self) -> bool {
        unsafe {
            let mut lastinput = LASTINPUTINFO {
                cbSize: size_of::<LASTINPUTINFO>() as u32,
                dwTime: 0,
            };
            GetLastInputInfo(&mut lastinput) != 0
                && GetTickCount().wrapping_sub(lastinput.dwTime) >= self.idle_timeout_ms
        }
    }
    fn set_idle_timeout(&mut self, ms: u32) {
        self.idle_timeout_ms = ms;
    }
}
