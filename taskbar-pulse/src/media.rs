//! "Now playing" information from Windows' System Media Transport Controls (SMTC).
//! Works with any app that integrates with the Windows media overlay
//! (Spotify, browsers playing YouTube, Media Player, etc.).

use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use windows::Media::Control::{
    GlobalSystemMediaTransportControlsSessionManager as Manager,
    GlobalSystemMediaTransportControlsSessionPlaybackStatus as Status,
};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct MediaInfo {
    pub title: String,
    pub artist: String,
    pub playing: bool,
}

pub enum MediaCommand {
    TogglePlayPause,
    Next,
    Previous,
    /// Change the polling interval (ms).
    Interval(u32),
    Quit,
}

fn read(manager: &Manager) -> Option<MediaInfo> {
    let session = manager.GetCurrentSession().ok()?;
    let props = session.TryGetMediaPropertiesAsync().ok()?.join().ok()?;
    let playing = session
        .GetPlaybackInfo()
        .and_then(|p| p.PlaybackStatus())
        .map(|s| s == Status::Playing)
        .unwrap_or(false);
    let title = props.Title().map(|s| s.to_string()).unwrap_or_default();
    if title.is_empty() {
        return None;
    }
    Some(MediaInfo {
        title,
        artist: props.Artist().map(|s| s.to_string()).unwrap_or_default(),
        playing,
    })
}

/// Runs on its own thread. Calls `changed` whenever the now-playing info changes.
pub fn run(
    shared: Arc<Mutex<Option<MediaInfo>>>,
    commands: Receiver<MediaCommand>,
    mut interval_ms: u32,
    changed: impl Fn(),
) {
    let manager = match Manager::RequestAsync().and_then(|op| op.join()) {
        Ok(m) => m,
        Err(_) => return, // SMTC unavailable (very old Windows 10 builds).
    };
    loop {
        let info = read(&manager);
        {
            let mut guard = shared.lock().unwrap();
            if *guard != info {
                *guard = info;
                drop(guard);
                changed();
            }
        }
        match commands.recv_timeout(Duration::from_millis(interval_ms as u64)) {
            Ok(MediaCommand::Quit) | Err(RecvTimeoutError::Disconnected) => return,
            Ok(MediaCommand::Interval(ms)) => interval_ms = ms,
            Ok(cmd) => {
                if let Ok(session) = manager.GetCurrentSession() {
                    let op = match cmd {
                        MediaCommand::TogglePlayPause => session.TryTogglePlayPauseAsync(),
                        MediaCommand::Next => session.TrySkipNextAsync(),
                        MediaCommand::Previous => session.TrySkipPreviousAsync(),
                        _ => continue,
                    };
                    if let Ok(op) = op {
                        let _ = op.join();
                    }
                    std::thread::sleep(Duration::from_millis(150));
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
        }
    }
}
