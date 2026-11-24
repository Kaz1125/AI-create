//! "Now playing" information from Windows' System Media Transport Controls (SMTC).
//! Works with any app that integrates with the Windows media overlay
//! (Spotify, browsers playing YouTube, Media Player, etc.).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// While switching tracks, SMTC can briefly report no session or an empty title.
/// Keep showing the previous track for this long before treating media as gone.
const GRACE: Duration = Duration::from_secs(6);

use windows::core::GUID;
use windows::Media::Control::{
    GlobalSystemMediaTransportControlsSession as Session,
    GlobalSystemMediaTransportControlsSessionManager as Manager,
    GlobalSystemMediaTransportControlsSessionPlaybackStatus as Status,
};
use windows::Win32::Graphics::Imaging::*;
use windows::Win32::System::Com::{CoCreateInstance, IStream, CLSCTX_INPROC_SERVER};
use windows::Win32::System::WinRT::CreateStreamOverRandomAccessStream;

/// Album art / thumbnail decoded to premultiplied BGRA, short side = ART_SIZE px.
#[derive(Debug)]
pub struct Art {
    pub id: u64,
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

impl PartialEq for Art {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

const ART_SIZE: u32 = 96;
static NEXT_ART_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug, Default, PartialEq)]
pub struct MediaInfo {
    pub title: String,
    pub artist: String,
    pub playing: bool,
    pub art: Option<Arc<Art>>,
}

pub enum MediaCommand {
    TogglePlayPause,
    Next,
    Previous,
    /// Change the polling interval (ms).
    Interval(u32),
    Quit,
}

fn decode_art(session: &Session) -> Option<Arc<Art>> {
    let props = session.TryGetMediaPropertiesAsync().ok()?.join().ok()?;
    let stream = props.Thumbnail().ok()?.OpenReadAsync().ok()?.join().ok()?;
    unsafe {
        let istream: IStream = CreateStreamOverRandomAccessStream(&stream).ok()?;
        let wic: IWICImagingFactory =
            CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).ok()?;
        let decoder = wic
            .CreateDecoderFromStream(&istream, std::ptr::null::<GUID>(), WICDecodeMetadataCacheOnDemand)
            .ok()?;
        let frame = decoder.GetFrame(0).ok()?;
        let (mut w, mut h) = (0u32, 0u32);
        frame.GetSize(&mut w, &mut h).ok()?;
        if w == 0 || h == 0 {
            return None;
        }
        // Scale so the short side is ART_SIZE; the renderer centre-crops to a square.
        let short = w.min(h) as f32;
        let (sw, sh) = (
            ((w as f32 * ART_SIZE as f32 / short).round() as u32).max(1),
            ((h as f32 * ART_SIZE as f32 / short).round() as u32).max(1),
        );
        let scaler = wic.CreateBitmapScaler().ok()?;
        scaler.Initialize(&frame, sw, sh, WICBitmapInterpolationModeFant).ok()?;
        let converter = wic.CreateFormatConverter().ok()?;
        converter
            .Initialize(&scaler, &GUID_WICPixelFormat32bppPBGRA, WICBitmapDitherTypeNone, None, 0.0, WICBitmapPaletteTypeCustom)
            .ok()?;
        let stride = sw * 4;
        let mut pixels = vec![0u8; (stride * sh) as usize];
        converter.CopyPixels(std::ptr::null(), stride, &mut pixels).ok()?;
        Some(Arc::new(Art { id: NEXT_ART_ID.fetch_add(1, Ordering::Relaxed), width: sw, height: sh, pixels }))
    }
}

fn read(manager: &Manager, previous: &Option<MediaInfo>, attempts: &mut u32) -> Option<MediaInfo> {
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
    let artist = props.Artist().map(|s| s.to_string()).unwrap_or_default();
    // Re-decode the thumbnail only when the track changes.
    // Some sources never provide one, so give up after a few tries per track.
    let same_track = matches!(previous, Some(p) if p.title == title && p.artist == artist);
    if !same_track {
        *attempts = 0;
    }
    let art = match previous {
        Some(p) if same_track && p.art.is_some() => p.art.clone(),
        _ if *attempts < 4 => {
            *attempts += 1;
            decode_art(&session)
        }
        _ => None,
    };
    Some(MediaInfo { title, artist, playing, art })
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
    let mut last: Option<MediaInfo> = None;
    let mut attempts = 0u32;
    let mut missing_since: Option<Instant> = None;
    // Poll faster for a few seconds after a skip so the new track shows up quickly.
    let mut fast_until: Option<Instant> = None;
    loop {
        let mut info = read(&manager, &last, &mut attempts);
        if info.is_none() && last.is_some() {
            let since = *missing_since.get_or_insert_with(Instant::now);
            if since.elapsed() < GRACE {
                info = last.clone();
            }
        } else {
            missing_since = None;
        }
        // Thumbnails often arrive a moment after the title: retry soon while missing.
        let retry_art = matches!(&info, Some(i) if i.art.is_none()) && attempts < 4;
        if info != last {
            last = info.clone();
            *shared.lock().unwrap() = info;
            changed();
        }
        let fast = fast_until.map_or(false, |t| Instant::now() < t) || missing_since.is_some();
        let wait = if fast { interval_ms.min(300) } else if retry_art { interval_ms.min(1500) } else { interval_ms };
        match commands.recv_timeout(Duration::from_millis(wait as u64)) {
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
                    fast_until = Some(Instant::now() + Duration::from_secs(3));
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
        }
    }
}
