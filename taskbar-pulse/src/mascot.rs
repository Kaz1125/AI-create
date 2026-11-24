//! Mini mascot: either a user-chosen image (animated GIF, PNG, JPEG, WebP...) or one of
//! the built-in procedurally drawn characters (see `render::draw_character`).

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use windows::core::{Interface, HSTRING, PCWSTR};
use windows::Win32::Foundation::GENERIC_READ;
use windows::Win32::Graphics::Imaging::*;
use windows::Win32::System::Com::StructuredStorage::{PropVariantToUInt16, PROPVARIANT};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};

/// Frames are stored scaled so their height is this many pixels (keeps memory small:
/// 100 frames of 96x96 BGRA is about 3.7 MB).
const FRAME_HEIGHT: u32 = 96;
const MAX_FRAMES: usize = 400;

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

pub struct Frame {
    pub pixels: Vec<u8>, // premultiplied BGRA
    /// Display time in milliseconds.
    pub delay_ms: u32,
}

pub struct Animation {
    pub id: u64,
    pub width: u32,
    pub height: u32,
    pub frames: Vec<Frame>,
    total_ms: u32,
}

impl Animation {
    /// Frame index to show `elapsed_ms` after the animation started (loops forever).
    pub fn frame_at(&self, elapsed_ms: u64) -> usize {
        if self.frames.len() <= 1 || self.total_ms == 0 {
            return 0;
        }
        let mut t = (elapsed_ms % self.total_ms as u64) as u32;
        for (i, f) in self.frames.iter().enumerate() {
            if t < f.delay_ms {
                return i;
            }
            t -= f.delay_ms;
        }
        self.frames.len() - 1
    }
}

fn meta_u16(reader: &IWICMetadataQueryReader, name: &str) -> Option<u16> {
    let mut value = PROPVARIANT::default();
    let name = HSTRING::from(name);
    unsafe {
        reader.GetMetadataByName(PCWSTR(name.as_ptr()), &mut value).ok()?;
        PropVariantToUInt16(&value).ok()
    }
}

/// Alpha-composites a premultiplied BGRA `src` (sw x sh) onto `dst` (dw wide) at (x, y).
fn blit_over(dst: &mut [u8], dw: u32, dh: u32, src: &[u8], sw: u32, sh: u32, x: u32, y: u32) {
    for row in 0..sh {
        let dy = y + row;
        if dy >= dh {
            break;
        }
        for col in 0..sw {
            let dx = x + col;
            if dx >= dw {
                break;
            }
            let s = ((row * sw + col) * 4) as usize;
            let d = ((dy * dw + dx) * 4) as usize;
            let a = src[s + 3] as u32;
            if a == 255 {
                dst[d..d + 4].copy_from_slice(&src[s..s + 4]);
            } else if a > 0 {
                for c in 0..4 {
                    dst[d + c] = (src[s + c] as u32 + dst[d + c] as u32 * (255 - a) / 255) as u8;
                }
            }
        }
    }
}

fn clear_rect(dst: &mut [u8], dw: u32, dh: u32, x: u32, y: u32, w: u32, h: u32) {
    for row in y..(y + h).min(dh) {
        let start = ((row * dw + x.min(dw)) * 4) as usize;
        let end = ((row * dw + (x + w).min(dw)) * 4) as usize;
        dst[start..end].fill(0);
    }
}

/// Converts any WIC source to premultiplied BGRA bytes.
unsafe fn to_pbgra(wic: &IWICImagingFactory, src: &IWICBitmapSource, w: u32, h: u32) -> Option<Vec<u8>> {
    let conv = wic.CreateFormatConverter().ok()?;
    conv.Initialize(src, &GUID_WICPixelFormat32bppPBGRA, WICBitmapDitherTypeNone, None, 0.0, WICBitmapPaletteTypeCustom)
        .ok()?;
    let mut buf = vec![0u8; (w * h * 4) as usize];
    conv.CopyPixels(std::ptr::null(), w * 4, &mut buf).ok()?;
    Some(buf)
}

/// Scales a full-size premultiplied BGRA canvas down to the stored frame size.
unsafe fn scale(wic: &IWICImagingFactory, canvas: &[u8], w: u32, h: u32, ow: u32, oh: u32) -> Option<Vec<u8>> {
    if w == ow && h == oh {
        return Some(canvas.to_vec());
    }
    let bmp = wic.CreateBitmapFromMemory(w, h, &GUID_WICPixelFormat32bppPBGRA, w * 4, canvas).ok()?;
    let scaler = wic.CreateBitmapScaler().ok()?;
    scaler.Initialize(&bmp, ow, oh, WICBitmapInterpolationModeFant).ok()?;
    let mut out = vec![0u8; (ow * oh * 4) as usize];
    scaler.CopyPixels(std::ptr::null(), ow * 4, &mut out).ok()?;
    Some(out)
}

/// Loads an image file. Animated GIFs are composited frame by frame (honouring frame
/// offsets, delays and disposal), so partial-update GIFs play correctly.
pub fn load(path: &Path) -> Option<Animation> {
    unsafe {
        let wic: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).ok()?;
        let wpath = HSTRING::from(path.as_os_str());
        let decoder = wic
            .CreateDecoderFromFilename(&wpath, None, GENERIC_READ, WICDecodeMetadataCacheOnDemand)
            .ok()?;
        let count = decoder.GetFrameCount().ok()?.max(1) as usize;
        let is_gif = decoder.GetContainerFormat().map(|g| g == GUID_ContainerFormatGif).unwrap_or(false);

        // Canvas size: GIF logical screen, otherwise the first frame's size.
        let first = decoder.GetFrame(0).ok()?;
        let (mut cw, mut ch) = (0u32, 0u32);
        first.GetSize(&mut cw, &mut ch).ok()?;
        if is_gif {
            if let Ok(meta) = decoder.GetMetadataQueryReader() {
                if let (Some(w), Some(h)) = (meta_u16(&meta, "/logscrdesc/Width"), meta_u16(&meta, "/logscrdesc/Height")) {
                    if w > 0 && h > 0 {
                        cw = w as u32;
                        ch = h as u32;
                    }
                }
            }
        }
        if cw == 0 || ch == 0 {
            return None;
        }
        let oh = FRAME_HEIGHT.min(ch);
        let ow = ((cw as f32 * oh as f32 / ch as f32).round() as u32).max(1);

        let mut canvas = vec![0u8; (cw * ch * 4) as usize];
        let mut frames = Vec::new();
        for i in 0..count.min(MAX_FRAMES) {
            let frame = decoder.GetFrame(i as u32).ok()?;
            let (mut fw, mut fh) = (0u32, 0u32);
            frame.GetSize(&mut fw, &mut fh).ok()?;
            let (mut left, mut top, mut delay, mut disposal) = (0u32, 0u32, 100u32, 0u16);
            if is_gif {
                if let Ok(meta) = frame.GetMetadataQueryReader() {
                    left = meta_u16(&meta, "/imgdesc/Left").unwrap_or(0) as u32;
                    top = meta_u16(&meta, "/imgdesc/Top").unwrap_or(0) as u32;
                    // GIF delay is in 1/100 s. Browsers treat 0-1 as 100 ms; do the same.
                    let d = meta_u16(&meta, "/grctlext/Delay").unwrap_or(10) as u32 * 10;
                    delay = if d <= 10 { 100 } else { d };
                    disposal = meta_u16(&meta, "/grctlext/Disposal").unwrap_or(0);
                }
            }
            let src: IWICBitmapSource = frame.cast().ok()?;
            let pixels = to_pbgra(&wic, &src, fw, fh)?;
            let before = (disposal == 3).then(|| canvas.clone());
            if !is_gif {
                canvas.fill(0);
            }
            blit_over(&mut canvas, cw, ch, &pixels, fw, fh, left, top);
            frames.push(Frame { pixels: scale(&wic, &canvas, cw, ch, ow, oh)?, delay_ms: delay });
            match disposal {
                2 => clear_rect(&mut canvas, cw, ch, left, top, fw, fh),
                3 => {
                    if let Some(b) = before {
                        canvas = b;
                    }
                }
                _ => {}
            }
        }
        let total_ms = frames.iter().map(|f| f.delay_ms).sum();
        Some(Animation { id: NEXT_ID.fetch_add(1, Ordering::Relaxed), width: ow, height: oh, frames, total_ms })
    }
}
