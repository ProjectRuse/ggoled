use super::{DEFAULT_IDLE_TIMEOUT_MS, Media, OSFeatures};
use std::mem::size_of;
use windows::Media::Control::{
    GlobalSystemMediaTransportControlsSession, GlobalSystemMediaTransportControlsSessionManager,
    GlobalSystemMediaTransportControlsSessionPlaybackStatus,
};
use windows_sys::Win32::{
    System::RemoteDesktop::{
        WTS_CURRENT_SERVER_HANDLE, WTS_CURRENT_SESSION, WTS_SESSIONSTATE_LOCK, WTS_SESSIONSTATE_UNLOCK, WTSFreeMemory,
        WTSINFOEXW, WTSQuerySessionInformationW, WTSSessionInfoEx,
    },
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
    fn is_locked(&mut self) -> Option<bool> {
        // Ask Windows for this session's lock state (what Task Manager and remote
        // desktop tools use). Correct on Windows 10/11; Windows 7 reported it inverted.
        unsafe {
            let mut buf: windows_sys::core::PWSTR = std::ptr::null_mut();
            let mut len = 0u32;
            if WTSQuerySessionInformationW(
                WTS_CURRENT_SERVER_HANDLE,
                WTS_CURRENT_SESSION,
                WTSSessionInfoEx,
                &mut buf,
                &mut len,
            ) == 0
                || buf.is_null()
            {
                return None;
            }
            let result = if (len as usize) >= size_of::<WTSINFOEXW>() {
                let info = &*(buf as *const WTSINFOEXW);
                match (info.Level, info.Data.WTSInfoExLevel1.SessionFlags as u32) {
                    (1, WTS_SESSIONSTATE_LOCK) => Some(true),
                    (1, WTS_SESSIONSTATE_UNLOCK) => Some(false),
                    _ => None,
                }
            } else {
                None
            };
            WTSFreeMemory(buf.cast());
            result
        }
    }
}
