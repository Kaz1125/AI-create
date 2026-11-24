//! Audio visualizer input: WASAPI loopback capture of the default playback device,
//! turned into log-spaced frequency bars with an FFT.

use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rustfft::{num_complex::Complex, FftPlanner};
use windows::Win32::Media::Audio::*;
use windows::Win32::System::Com::*;

const FFT_SIZE: usize = 2048;
const HOP: usize = 512;

pub struct Visualizer {
    pub bars: Arc<Mutex<Vec<f32>>>,
    pub enabled: Arc<AtomicBool>,
    pub bar_count: Arc<AtomicUsize>,
    /// 0 = normal, 1 = high, 2 = auto gain. See `config::Sensitivity`.
    pub sensitivity: Arc<AtomicU8>,
}

impl Visualizer {
    pub fn start(bar_count: usize, enabled: bool, sensitivity: u8) -> Visualizer {
        let v = Visualizer {
            bars: Arc::new(Mutex::new(vec![0.0; bar_count])),
            enabled: Arc::new(AtomicBool::new(enabled)),
            bar_count: Arc::new(AtomicUsize::new(bar_count)),
            sensitivity: Arc::new(AtomicU8::new(sensitivity)),
        };
        let (bars, en, count, sens) =
            (v.bars.clone(), v.enabled.clone(), v.bar_count.clone(), v.sensitivity.clone());
        std::thread::Builder::new()
            .name("audio".into())
            .spawn(move || {
                unsafe {
                    let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
                }
                loop {
                    if !en.load(Ordering::Relaxed) {
                        bars.lock().unwrap().iter_mut().for_each(|b| *b = 0.0);
                        std::thread::sleep(Duration::from_millis(300));
                        continue;
                    }
                    // Returns when the device changes/disappears or the visualizer is disabled.
                    let _ = capture(&bars, &en, &count, &sens);
                    std::thread::sleep(Duration::from_millis(500));
                }
            })
            .expect("spawn audio thread");
        v
    }

    pub fn snapshot(&self) -> Vec<f32> {
        self.bars.lock().unwrap().clone()
    }
}

struct Format {
    channels: usize,
    sample_rate: f32,
    float: bool,
    bits: u16,
}

unsafe fn mix_format(client: &IAudioClient) -> windows::core::Result<(*mut WAVEFORMATEX, Format)> {
    let fmt = client.GetMixFormat()?;
    let f = &*fmt;
    let tag = f.wFormatTag as u32;
    // WAVE_FORMAT_IEEE_FLOAT = 3, WAVE_FORMAT_EXTENSIBLE = 0xFFFE (shared-mode mix format is
    // normally 32-bit float inside EXTENSIBLE).
    let float = tag == 3 || (tag == 0xFFFE && f.wBitsPerSample == 32);
    Ok((
        fmt,
        Format {
            channels: f.nChannels.max(1) as usize,
            sample_rate: f.nSamplesPerSec as f32,
            float,
            bits: f.wBitsPerSample,
        },
    ))
}

fn capture(
    bars: &Mutex<Vec<f32>>,
    enabled: &AtomicBool,
    count: &AtomicUsize,
    sensitivity: &AtomicU8,
) -> windows::core::Result<()> {
    unsafe {
        let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let device = enumerator.GetDefaultAudioEndpoint(eRender, eConsole)?;
        let initial_id = device.GetId()?.to_string().unwrap_or_default();
        let client: IAudioClient = device.Activate(CLSCTX_ALL, None)?;
        let (fmt_ptr, fmt) = mix_format(&client)?;
        let result = client.Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_LOOPBACK,
            2_000_000, // 200 ms buffer (100 ns units)
            0,
            fmt_ptr,
            None,
        );
        CoTaskMemFree(Some(fmt_ptr as _));
        result?;
        let capture: IAudioCaptureClient = client.GetService()?;
        client.Start()?;

        let mut planner = FftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(FFT_SIZE);
        let window: Vec<f32> = (0..FFT_SIZE)
            .map(|i| 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / (FFT_SIZE - 1) as f32).cos())
            .collect();
        let mut ring: Vec<f32> = Vec::with_capacity(FFT_SIZE * 2);
        let mut since_fft = 0usize;
        let mut smooth: Vec<f32> = Vec::new();
        let mut buffer = vec![Complex::new(0.0f32, 0.0); FFT_SIZE];
        let mut checks = 0u32;
        let mut silent_ticks = 0u32;
        let mut gain = AutoGain::new();

        let outcome = loop {
            if !enabled.load(Ordering::Relaxed) {
                break Ok(());
            }
            std::thread::sleep(Duration::from_millis(10));

            // Every ~2 s, restart if the default output device changed.
            checks += 1;
            if checks % 200 == 0 {
                if let Ok(d) = enumerator.GetDefaultAudioEndpoint(eRender, eConsole) {
                    if d.GetId().ok().and_then(|id| id.to_string().ok()).unwrap_or_default() != initial_id {
                        break Ok(());
                    }
                }
            }

            let mut got_audio = false;
            loop {
                let packet = match capture.GetNextPacketSize() {
                    Ok(p) => p,
                    Err(e) => {
                        let _ = client.Stop();
                        return Err(e);
                    }
                };
                if packet == 0 {
                    break;
                }
                let mut data = std::ptr::null_mut();
                let mut frames = 0u32;
                let mut flags = 0u32;
                capture.GetBuffer(&mut data, &mut frames, &mut flags, None, None)?;
                let n = frames as usize;
                let silent = flags & (AUDCLNT_BUFFERFLAGS_SILENT.0 as u32) != 0;
                for i in 0..n {
                    let mut sum = 0.0f32;
                    if !silent {
                        for c in 0..fmt.channels {
                            let idx = i * fmt.channels + c;
                            sum += if fmt.float {
                                *(data as *const f32).add(idx)
                            } else if fmt.bits == 16 {
                                *(data as *const i16).add(idx) as f32 / 32768.0
                            } else {
                                0.0
                            };
                        }
                    }
                    ring.push(sum / fmt.channels as f32);
                }
                capture.ReleaseBuffer(frames)?;
                since_fft += n;
                got_audio = true;
            }
            if ring.len() > FFT_SIZE {
                let excess = ring.len() - FFT_SIZE;
                ring.drain(..excess);
            }

            let bar_count = count.load(Ordering::Relaxed).max(1);
            if smooth.len() != bar_count {
                smooth = vec![0.0; bar_count];
            }

            if got_audio && since_fft >= HOP && ring.len() == FFT_SIZE {
                since_fft = 0;
                silent_ticks = 0;
                for (i, s) in ring.iter().enumerate() {
                    buffer[i] = Complex::new(s * window[i], 0.0);
                }
                fft.process(&mut buffer);
                let db = bands_db(&buffer, fmt.sample_rate, bar_count);
                let target = gain.scale(&db, sensitivity.load(Ordering::Relaxed));
                for (s, t) in smooth.iter_mut().zip(target) {
                    // Fast attack, slower release.
                    *s = if t > *s { *s + (t - *s) * 0.6 } else { *s + (t - *s) * 0.25 };
                }
            } else if !got_audio {
                // Loopback delivers no packets while nothing plays: let the bars fall.
                silent_ticks += 1;
                if silent_ticks > 3 {
                    smooth.iter_mut().for_each(|s| *s *= 0.85);
                }
            }
            *bars.lock().unwrap() = smooth.clone();
        };
        let _ = client.Stop();
        outcome
    }
}

/// Groups FFT bins into `count` log-spaced bands between 40 Hz and 16 kHz, in dBFS.
fn bands_db(spectrum: &[Complex<f32>], sample_rate: f32, count: usize) -> Vec<f32> {
    let half = FFT_SIZE / 2;
    let bin_hz = sample_rate / FFT_SIZE as f32;
    let (lo, hi) = (40.0f32, 16_000.0f32.min(sample_rate / 2.0));
    (0..count)
        .map(|b| {
            let f0 = lo * (hi / lo).powf(b as f32 / count as f32);
            let f1 = lo * (hi / lo).powf((b + 1) as f32 / count as f32);
            let i0 = ((f0 / bin_hz) as usize).clamp(1, half - 1);
            let i1 = ((f1 / bin_hz) as usize).clamp(i0 + 1, half);
            let peak = spectrum[i0..i1].iter().map(|c| c.norm()).fold(0.0f32, f32::max);
            let mag = peak / (FFT_SIZE as f32 / 4.0);
            20.0 * mag.max(1e-9).log10()
        })
        .collect()
}

/// Maps band levels (dBFS) to bar heights 0..1.
///
/// - Normal (0): fixed window -60..0 dBFS.
/// - High (1): fixed window -85..-25 dBFS, for quiet playback.
/// - Auto (2): follows the loudest recent band so quiet and loud music both fill the bars.
///   The reference rises instantly and falls about 6 dB per second; anything quieter than
///   -110 dBFS is treated as silence so background noise is not amplified.
struct AutoGain {
    reference_db: f32,
}

impl AutoGain {
    const RANGE_DB: f32 = 48.0;
    const FALL_PER_FRAME_DB: f32 = 0.065; // frames arrive ~94 times/s at 48 kHz
    const SILENCE_DB: f32 = -110.0;

    fn new() -> AutoGain {
        AutoGain { reference_db: -40.0 }
    }

    fn scale(&mut self, db: &[f32], mode: u8) -> Vec<f32> {
        let (floor, range) = match mode {
            0 => (-60.0, 60.0),
            1 => (-85.0, 60.0),
            _ => {
                let loudest = db.iter().cloned().fold(f32::MIN, f32::max);
                self.reference_db = if loudest > self.reference_db {
                    loudest
                } else {
                    // Never chase below -70 dBFS, so near-silence does not become full-height bars.
                    (self.reference_db - Self::FALL_PER_FRAME_DB).max(-70.0)
                };
                if loudest < Self::SILENCE_DB {
                    return vec![0.0; db.len()];
                }
                (self.reference_db - Self::RANGE_DB, Self::RANGE_DB)
            }
        };
        db.iter().map(|d| ((d - floor) / range).clamp(0.0, 1.0)).collect()
    }
}
