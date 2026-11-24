//! Draws the overlay with Direct2D + DirectWrite into a 32-bit DIB, then hands it to
//! UpdateLayeredWindow for per-pixel transparency over the taskbar.
//!
//! Low-load modes use Direct2D's software (CPU) rasterizer; Standard/Performance modes ask
//! for a hardware (GPU) render target and fall back to software if that fails.

use std::sync::Arc;
use std::time::Instant;

use windows::core::{w, Result, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows_numerics::{Matrix3x2, Vector2};

use crate::anim::Visual;
use crate::battery_kind_glyph;
use crate::config::{Character, Config, Item, Palette, VizStyle};
use crate::mascot::Animation;
use crate::media::{Art, MediaInfo};

/// One horizontal piece of the overlay.
pub enum Segment {
    /// Small label followed by a bold value, e.g. "CPU" "32°".
    Metric {
        icon: Option<char>,
        label: String,
        value: String,
        /// Value colour (0xRRGGBB) when colouring by value or warning; None = theme text.
        color: Option<u32>,
        /// Sparkline samples normalised to 0..1, oldest first.
        history: Option<Vec<f32>>,
    },
    Media {
        playing: bool,
        text: String,
        art: Option<Arc<Art>>,
    },
    Visualizer {
        bars: Vec<f32>,
        peaks: Option<Vec<f32>>,
    },
    /// Built-in character at animation phase `phase` (seconds of motion).
    Character { kind: Character, phase: f32 },
    /// One frame of a user image / GIF.
    Image { anim: Arc<Animation>, frame: usize },
}

pub struct Renderer {
    d2d: ID2D1Factory,
    dwrite: IDWriteFactory,
    target: Option<ID2D1DCRenderTarget>,
    hardware: bool,
    using_hardware: bool,
    label_fmt: Option<IDWriteTextFormat>,
    value_fmt: Option<IDWriteTextFormat>,
    icon_fmt: Option<IDWriteTextFormat>,
    fmt_key: u32,
    dib: Option<(HDC, HBITMAP, HGDIOBJ, i32, i32)>,
    /// Album art bitmap for the current render target, keyed by Art::id.
    art_cache: Option<(u64, ID2D1Bitmap)>,
    /// Mascot image frames uploaded to the current render target, keyed by Animation::id.
    gif_cache: Option<(u64, Vec<Option<ID2D1Bitmap>>)>,
    started: Instant,
    /// Horizontal range of the media segment in window pixels (for click handling).
    pub media_hit: Option<(f32, f32)>,
    /// True when the last frame had a scrolling title (needs animation frames).
    pub marquee_active: bool,
}

fn color(rgb: u32, a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: ((rgb >> 16) & 0xFF) as f32 / 255.0,
        g: ((rgb >> 8) & 0xFF) as f32 / 255.0,
        b: (rgb & 0xFF) as f32 / 255.0,
        a,
    }
}

fn pt(x: f32, y: f32) -> Vector2 {
    Vector2 { X: x, Y: y }
}

fn rect(l: f32, t: f32, r: f32, b: f32) -> D2D_RECT_F {
    D2D_RECT_F { left: l, top: t, right: r, bottom: b }
}

fn rounded(l: f32, t: f32, r: f32, b: f32, radius: f32) -> D2D1_ROUNDED_RECT {
    D2D1_ROUNDED_RECT { rect: rect(l, t, r, b), radiusX: radius, radiusY: radius }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

fn lerp_rgb(a: u32, b: u32, t: f32) -> u32 {
    let t = t.clamp(0.0, 1.0);
    let ch = |shift: u32| {
        let (x, y) = (((a >> shift) & 0xFF) as f32, ((b >> shift) & 0xFF) as f32);
        ((x + (y - x) * t).round() as u32) << shift
    };
    ch(16) | ch(8) | ch(0)
}

/// Green -> yellow -> red for t in 0..1.
pub fn ramp(t: f32) -> u32 {
    const GREEN: u32 = 0x5BE38A;
    const YELLOW: u32 = 0xFFD24D;
    const RED: u32 = 0xFF5A5A;
    if t < 0.5 {
        lerp_rgb(GREEN, YELLOW, t * 2.0)
    } else {
        lerp_rgb(YELLOW, RED, (t - 0.5) * 2.0)
    }
}

const VIZ_STEP: f32 = 4.5;
const VIZ_BAR: f32 = 3.0;
const MARQUEE_SPEED: f32 = 28.0; // DIPs per second
const MARQUEE_GAP: f32 = 36.0;

impl Renderer {
    pub fn new() -> Result<Renderer> {
        unsafe {
            let d2d: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
            let dwrite: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            Ok(Renderer {
                d2d,
                dwrite,
                target: None,
                hardware: false,
                using_hardware: false,
                label_fmt: None,
                value_fmt: None,
                icon_fmt: None,
                fmt_key: 0,
                dib: None,
                art_cache: None,
                gif_cache: None,
                started: Instant::now(),
                media_hit: None,
                marquee_active: false,
            })
        }
    }

    /// Switches between GPU and CPU rasterization (takes effect on the next frame).
    pub fn set_hardware(&mut self, hardware: bool) {
        if self.hardware != hardware {
            self.hardware = hardware;
            self.drop_target();
        }
    }

    fn drop_target(&mut self) {
        self.target = None;
        self.art_cache = None; // bitmaps belong to the render target
        self.gif_cache = None;
    }

    pub fn is_hardware(&self) -> bool {
        self.using_hardware
    }

    fn make_target(&mut self) -> Result<ID2D1DCRenderTarget> {
        let props = |kind| D2D1_RENDER_TARGET_PROPERTIES {
            r#type: kind,
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
            },
            dpiX: 96.0,
            dpiY: 96.0,
            usage: D2D1_RENDER_TARGET_USAGE_NONE,
            minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
        };
        unsafe {
            if self.hardware {
                if let Ok(t) = self.d2d.CreateDCRenderTarget(&props(D2D1_RENDER_TARGET_TYPE_HARDWARE)) {
                    self.using_hardware = true;
                    return Ok(t);
                }
            }
            self.using_hardware = false;
            self.d2d.CreateDCRenderTarget(&props(D2D1_RENDER_TARGET_TYPE_SOFTWARE))
        }
    }

    fn icon_family(&self) -> PCWSTR {
        // Segoe Fluent Icons ships with Windows 11; Segoe MDL2 Assets with Windows 10.
        unsafe {
            let mut collection = None;
            if self.dwrite.GetSystemFontCollection(&mut collection, false).is_ok() {
                if let Some(c) = collection {
                    let mut index = 0u32;
                    let mut exists = windows::core::BOOL(0);
                    if c.FindFamilyName(w!("Segoe Fluent Icons"), &mut index, &mut exists).is_ok()
                        && exists.as_bool()
                    {
                        return w!("Segoe Fluent Icons");
                    }
                }
            }
        }
        w!("Segoe MDL2 Assets")
    }

    fn ensure_formats(&mut self, size: f32) -> Result<()> {
        let key = size.to_bits();
        if self.fmt_key == key && self.value_fmt.is_some() {
            return Ok(());
        }
        unsafe {
            let make = |family: PCWSTR, weight, size: f32| {
                self.dwrite.CreateTextFormat(
                    family,
                    None,
                    weight,
                    DWRITE_FONT_STYLE_NORMAL,
                    DWRITE_FONT_STRETCH_NORMAL,
                    size,
                    w!("ja-JP"),
                )
            };
            self.label_fmt = Some(make(w!("Segoe UI"), DWRITE_FONT_WEIGHT_SEMI_BOLD, size * 0.78)?);
            self.value_fmt = Some(make(w!("Segoe UI"), DWRITE_FONT_WEIGHT_BOLD, size * 1.18)?);
            self.icon_fmt = Some(make(self.icon_family(), DWRITE_FONT_WEIGHT_NORMAL, size * 1.1)?);
            for f in [&self.label_fmt, &self.value_fmt, &self.icon_fmt].into_iter().flatten() {
                f.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
            }
        }
        self.fmt_key = key;
        Ok(())
    }

    fn layout(&self, text: &str, fmt: &IDWriteTextFormat) -> Result<(IDWriteTextLayout, f32, f32)> {
        unsafe {
            let layout = self.dwrite.CreateTextLayout(&wide(text), fmt, 8192.0, 256.0)?;
            let mut m = DWRITE_TEXT_METRICS::default();
            layout.GetMetrics(&mut m)?;
            Ok((layout, m.widthIncludingTrailingWhitespace, m.height))
        }
    }

    fn ensure_dib(&mut self, w: i32, h: i32) -> Result<HDC> {
        if let Some((dc, _, _, dw, dh)) = self.dib {
            if dw == w && dh == h {
                return Ok(dc);
            }
        }
        self.free_dib();
        unsafe {
            let screen = GetDC(None);
            let dc = CreateCompatibleDC(Some(screen));
            ReleaseDC(None, screen);
            let bmi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: w,
                    biHeight: -h, // top-down
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits = std::ptr::null_mut();
            let bmp = CreateDIBSection(Some(dc), &bmi, DIB_RGB_COLORS, &mut bits, None, 0)?;
            let old = SelectObject(dc, bmp.into());
            self.dib = Some((dc, bmp, old, w, h));
            Ok(dc)
        }
    }

    fn free_dib(&mut self) {
        if let Some((dc, bmp, old, _, _)) = self.dib.take() {
            unsafe {
                SelectObject(dc, old);
                let _ = DeleteObject(bmp.into());
                let _ = DeleteDC(dc);
            }
        }
    }

    /// Width of each segment (including the gap after it), in pixels.
    fn measure(&self, segs: &[Segment], h: f32, scale: f32, cfg: &Config) -> Result<Vec<f32>> {
        let gap = 14.0 * scale;
        let mut widths = Vec::with_capacity(segs.len());
        for s in segs {
            let w = match s {
                Segment::Metric { icon, label, value, .. } => {
                    let mut w = 0.0;
                    if let Some(c) = icon {
                        w += self.layout(&c.to_string(), self.icon_fmt.as_ref().unwrap())?.1 + 4.0 * scale;
                    }
                    if !label.is_empty() {
                        w += self.layout(label, self.label_fmt.as_ref().unwrap())?.1 + 4.0 * scale;
                    }
                    w + self.layout(value, self.value_fmt.as_ref().unwrap())?.1
                }
                Segment::Media { text, art, .. } => {
                    let art_w = if art.is_some() { art_size(h) + 7.0 * scale } else { 0.0 };
                    let icon_w = self.layout("\u{E768}", self.icon_fmt.as_ref().unwrap())?.1 + 5.0 * scale;
                    let text_w = self.layout(text, self.label_fmt.as_ref().unwrap())?.1;
                    let max = cfg.media_width * scale;
                    art_w + icon_w + if cfg.marquee { text_w.min(max) } else { text_w }
                }
                Segment::Visualizer { bars, .. } => bars.len() as f32 * VIZ_STEP * scale,
                Segment::Character { .. } => mascot_size(h),
                Segment::Image { anim, .. } => mascot_size(h) * anim.width as f32 / anim.height.max(1) as f32,
            };
            widths.push(w + gap);
        }
        Ok(widths)
    }

    fn art_bitmap(&mut self, target: &ID2D1DCRenderTarget, art: &Art) -> Option<ID2D1Bitmap> {
        if let Some((id, bmp)) = &self.art_cache {
            if *id == art.id {
                return Some(bmp.clone());
            }
        }
        let props = D2D1_BITMAP_PROPERTIES {
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
            },
            dpiX: 96.0,
            dpiY: 96.0,
        };
        let bmp = unsafe {
            target
                .CreateBitmap(
                    D2D_SIZE_U { width: art.width, height: art.height },
                    Some(art.pixels.as_ptr() as _),
                    art.width * 4,
                    &props,
                )
                .ok()?
        };
        self.art_cache = Some((art.id, bmp.clone()));
        Some(bmp)
    }

    /// Renders the segments. Returns the DIB's DC and size for UpdateLayeredWindow.
    pub fn draw(&mut self, segs: &[Segment], height: i32, scale: f32, cfg: &Config) -> Result<(HDC, i32, i32)> {
        self.ensure_formats(cfg.font_size * scale)?;
        let h = height as f32;
        let widths = self.measure(segs, h, scale, cfg)?;
        let pad = if cfg.panel { 12.0 * scale } else { 3.0 * scale };
        let total = (widths.iter().sum::<f32>() + pad * 2.0 - 14.0 * scale).ceil().max(8.0) as i32;
        let dc = self.ensure_dib(total, height)?;
        if self.target.is_none() {
            self.target = Some(self.make_target()?);
        }
        let target = self.target.clone().unwrap();
        let pal = cfg.palette();
        let time = self.started.elapsed().as_secs_f32();
        self.media_hit = None;
        self.marquee_active = false;
        unsafe {
            target.BindDC(dc, &RECT { left: 0, top: 0, right: total, bottom: height })?;
            target.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
            target.BeginDraw();
            // Alpha 1/255 keeps the whole strip clickable while looking fully transparent.
            target.Clear(Some(&D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 1.0 / 255.0 }));

            if cfg.panel {
                self.draw_panel(&target, total as f32, h, scale, &pal, cfg.panel_opacity)?;
            }

            let mut x = pad;
            for (seg, w) in segs.iter().zip(&widths) {
                let seg_w = w - 14.0 * scale;
                match seg {
                    Segment::Metric { icon, label, value, color: c, history } => {
                        let line = c.unwrap_or(pal.gradient[0]);
                        if let Some(hist) = history {
                            self.draw_sparkline(&target, hist, x, seg_w, h, scale, line)?;
                        }
                        let text = target.CreateSolidColorBrush(&color(pal.text, 1.0), None)?;
                        let label_b = target.CreateSolidColorBrush(&color(pal.label, 1.0), None)?;
                        let value_b = target.CreateSolidColorBrush(&color(c.unwrap_or(pal.text), 1.0), None)?;
                        let mut cx = x;
                        if let Some(ch) = icon {
                            let (lay, iw, ih) = self.layout(&ch.to_string(), self.icon_fmt.as_ref().unwrap())?;
                            target.DrawTextLayout(pt(cx, (h - ih) / 2.0), &lay, &text, D2D1_DRAW_TEXT_OPTIONS_NONE);
                            cx += iw + 4.0 * scale;
                        }
                        if !label.is_empty() {
                            let (lay, lw, lh) = self.layout(label, self.label_fmt.as_ref().unwrap())?;
                            target.DrawTextLayout(pt(cx, (h - lh) / 2.0 + scale), &lay, &label_b, D2D1_DRAW_TEXT_OPTIONS_NONE);
                            cx += lw + 4.0 * scale;
                        }
                        let (lay, _, vh) = self.layout(value, self.value_fmt.as_ref().unwrap())?;
                        target.DrawTextLayout(pt(cx, (h - vh) / 2.0), &lay, &value_b, D2D1_DRAW_TEXT_OPTIONS_NONE);
                    }
                    Segment::Media { playing, text: t, art } => {
                        self.draw_media(&target, x, seg_w, h, scale, cfg, &pal, *playing, t, art.as_deref(), time)?;
                        self.media_hit = Some((x, x + seg_w));
                    }
                    Segment::Visualizer { bars, peaks } => {
                        self.draw_visualizer(&target, x, seg_w, h, scale, cfg.visualizer_style, &pal, bars, peaks.as_deref())?;
                    }
                    Segment::Character { kind, phase } => {
                        self.draw_character(&target, x, h, scale, &pal, *kind, *phase)?;
                    }
                    Segment::Image { anim, frame } => {
                        if let Some(bmp) = self.gif_bitmap(&target, anim, *frame) {
                            let sz = mascot_size(h);
                            let top = (h - sz) / 2.0;
                            target.DrawBitmap(
                                &bmp,
                                Some(&rect(x, top, x + seg_w, top + sz)),
                                1.0,
                                D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
                                None,
                            );
                        }
                    }
                }
                x += w;
            }
            if let Err(e) = target.EndDraw(None, None) {
                // Device lost (e.g. GPU driver reset): recreate next frame.
                self.drop_target();
                return Err(e);
            }
        }
        Ok((dc, total, height))
    }

    fn gif_bitmap(&mut self, target: &ID2D1DCRenderTarget, anim: &Animation, frame: usize) -> Option<ID2D1Bitmap> {
        if self.gif_cache.as_ref().map(|(id, _)| *id) != Some(anim.id) {
            self.gif_cache = Some((anim.id, vec![None; anim.frames.len()]));
        }
        let (_, cache) = self.gif_cache.as_mut()?;
        if let Some(b) = cache.get(frame)?.clone() {
            return Some(b);
        }
        let props = D2D1_BITMAP_PROPERTIES {
            pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
            dpiX: 96.0,
            dpiY: 96.0,
        };
        let f = &anim.frames[frame];
        let bmp = unsafe {
            target
                .CreateBitmap(
                    D2D_SIZE_U { width: anim.width, height: anim.height },
                    Some(f.pixels.as_ptr() as _),
                    anim.width * 4,
                    &props,
                )
                .ok()?
        };
        cache[frame] = Some(bmp.clone());
        Some(bmp)
    }

    /// Original procedurally drawn mascots, tinted with the theme gradient.
    #[allow(clippy::too_many_arguments)]
    unsafe fn draw_character(
        &self,
        target: &ID2D1DCRenderTarget,
        x: f32,
        h: f32,
        scale: f32,
        pal: &Palette,
        kind: Character,
        t: f32,
    ) -> Result<()> {
        use std::f32::consts::PI;
        let sz = mascot_size(h);
        let top = (h - sz) / 2.0;
        let cx = x + sz / 2.0;
        let body = self.vertical_gradient(target, top, top + sz, &[(0.0, pal.gradient[0], 1.0), (1.0, pal.gradient[1], 1.0)])?;
        let eye = target.CreateSolidColorBrush(&color(0x1A1A24, 1.0), None)?;
        let shine = target.CreateSolidColorBrush(&color(0xFFFFFF, 0.85), None)?;
        let cheek = target.CreateSolidColorBrush(&color(pal.gradient[2], 0.55), None)?;
        let ellipse = |px: f32, py: f32, rx: f32, ry: f32| D2D1_ELLIPSE { point: pt(px, py), radiusX: rx, radiusY: ry };
        // Blink for 0.12 s every 3.5 s.
        let blink = (t % 3.5) < 0.12;
        let eyes = |ex: f32, ey: f32, gap: f32, r: f32| {
            for side in [-1.0, 1.0] {
                let px = ex + side * gap;
                if blink {
                    target.FillRoundedRectangle(&rounded(px - r, ey - 0.6 * scale, px + r, ey + 0.6 * scale, 0.6 * scale), &eye);
                } else {
                    target.FillEllipse(&ellipse(px, ey, r, r * 1.15), &eye);
                    target.FillEllipse(&ellipse(px - r * 0.35, ey - r * 0.4, r * 0.38, r * 0.38), &shine);
                }
            }
        };

        match kind {
            Character::Slime | Character::Image => {
                // Bounce: up in a sine arc, squash on landing.
                let cycle = (t * 1.6) % 1.0;
                let hop = (cycle * PI).sin();
                let squash = if hop < 0.15 { 1.0 - (0.15 - hop) * 1.6 } else { 1.0 };
                let w = sz * 0.46 / squash.sqrt();
                let hh = sz * 0.36 * squash;
                let ground = top + sz - 1.0 * scale;
                let cy = ground - hh - hop * sz * 0.18;
                let shadow = target.CreateSolidColorBrush(&color(0x000000, 0.25 * (1.0 - hop * 0.6)), None)?;
                target.FillEllipse(&ellipse(cx, ground, w * (0.8 - hop * 0.25), 1.6 * scale), &shadow);
                // Dome-shaped body: ellipse plus a flat-ish bottom.
                target.FillEllipse(&ellipse(cx, cy + hh * 0.15, w, hh), &body);
                target.FillEllipse(&ellipse(cx - w * 0.45, cy - hh * 0.35, w * 0.16, hh * 0.18), &shine);
                eyes(cx, cy + hh * 0.05, w * 0.32, sz * 0.055);
                target.FillEllipse(&ellipse(cx - w * 0.62, cy + hh * 0.32, sz * 0.06, sz * 0.035), &cheek);
                target.FillEllipse(&ellipse(cx + w * 0.62, cy + hh * 0.32, sz * 0.06, sz * 0.035), &cheek);
            }
            Character::Cat => {
                let bob = (t * 2.2 * PI).sin() * sz * 0.04;
                let r = sz * 0.34;
                let cy = top + sz * 0.58 + bob;
                // Ears: triangles; the right ear twitches now and then.
                let twitch = if (t % 4.7) < 0.18 { -0.25 } else { 0.0 };
                for (side, tilt) in [(-1.0f32, 0.0f32), (1.0, twitch)] {
                    let g = self.d2d.CreatePathGeometry()?;
                    let sink = g.Open()?;
                    let bx = cx + side * r * 0.62;
                    sink.BeginFigure(pt(bx - side * r * 0.42, cy - r * 0.55), D2D1_FIGURE_BEGIN_FILLED);
                    sink.AddLine(pt(bx + side * r * (0.28 + tilt), cy - r * (1.35 + tilt * 0.4)));
                    sink.AddLine(pt(bx + side * r * 0.38, cy - r * 0.25));
                    sink.EndFigure(D2D1_FIGURE_END_CLOSED);
                    sink.Close()?;
                    target.FillGeometry(&g, &body, None);
                }
                target.FillEllipse(&ellipse(cx, cy, r * 1.08, r * 0.92), &body);
                eyes(cx, cy - r * 0.08, r * 0.42, sz * 0.05);
                // Nose and whisker-ish mouth.
                target.FillEllipse(&ellipse(cx, cy + r * 0.22, sz * 0.03, sz * 0.022), &cheek);
                let mouth = target.CreateSolidColorBrush(&color(0x1A1A24, 0.7), None)?;
                target.DrawLine(pt(cx, cy + r * 0.26), pt(cx - r * 0.16, cy + r * 0.4), &mouth, 0.9 * scale, None);
                target.DrawLine(pt(cx, cy + r * 0.26), pt(cx + r * 0.16, cy + r * 0.4), &mouth, 0.9 * scale, None);
                target.FillEllipse(&ellipse(cx - r * 0.66, cy + r * 0.25, sz * 0.06, sz * 0.035), &cheek);
                target.FillEllipse(&ellipse(cx + r * 0.66, cy + r * 0.25, sz * 0.06, sz * 0.035), &cheek);
            }
            Character::Ghost => {
                let float = (t * 1.3 * PI).sin() * sz * 0.07;
                let w = sz * 0.36;
                let head_y = top + sz * 0.38 + float;
                let hem_y = top + sz * 0.86 + float;
                let g = self.d2d.CreatePathGeometry()?;
                let sink = g.Open()?;
                sink.BeginFigure(pt(cx - w, hem_y), D2D1_FIGURE_BEGIN_FILLED);
                sink.AddLine(pt(cx - w, head_y));
                // Round head: two quadratic arcs.
                sink.AddQuadraticBezier(&D2D1_QUADRATIC_BEZIER_SEGMENT { point1: pt(cx - w, head_y - w * 1.25), point2: pt(cx, head_y - w * 1.25) });
                sink.AddQuadraticBezier(&D2D1_QUADRATIC_BEZIER_SEGMENT { point1: pt(cx + w, head_y - w * 1.25), point2: pt(cx + w, head_y) });
                sink.AddLine(pt(cx + w, hem_y));
                // Wavy hem that ripples over time.
                let waves = 3;
                for i in 0..waves {
                    let x0 = cx + w - (i as f32) * (2.0 * w / waves as f32);
                    let x1 = x0 - 2.0 * w / waves as f32;
                    let dip = (t * 4.0 + i as f32 * 1.7).sin() * sz * 0.05 + sz * 0.07;
                    sink.AddQuadraticBezier(&D2D1_QUADRATIC_BEZIER_SEGMENT { point1: pt((x0 + x1) / 2.0, hem_y + dip), point2: pt(x1, hem_y) });
                }
                sink.EndFigure(D2D1_FIGURE_END_CLOSED);
                sink.Close()?;
                body.SetOpacity(0.92);
                target.FillGeometry(&g, &body, None);
                body.SetOpacity(1.0);
                eyes(cx, head_y - w * 0.15, w * 0.38, sz * 0.055);
                target.FillEllipse(&ellipse(cx, head_y + w * 0.25, sz * 0.035, sz * 0.045), &eye);
            }
        }
        Ok(())
    }

    unsafe fn gradient(&self, target: &ID2D1DCRenderTarget, x0: f32, x1: f32, stops: &[(f32, u32, f32)]) -> Result<ID2D1LinearGradientBrush> {
        let stops: Vec<D2D1_GRADIENT_STOP> =
            stops.iter().map(|(p, c, a)| D2D1_GRADIENT_STOP { position: *p, color: color(*c, *a) }).collect();
        let collection = target.CreateGradientStopCollection(&stops, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP)?;
        target.CreateLinearGradientBrush(
            &D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES { startPoint: pt(x0, 0.0), endPoint: pt(x1, 0.0) },
            None,
            &collection,
        )
    }

    unsafe fn vertical_gradient(&self, target: &ID2D1DCRenderTarget, y0: f32, y1: f32, stops: &[(f32, u32, f32)]) -> Result<ID2D1LinearGradientBrush> {
        let stops: Vec<D2D1_GRADIENT_STOP> =
            stops.iter().map(|(p, c, a)| D2D1_GRADIENT_STOP { position: *p, color: color(*c, *a) }).collect();
        let collection = target.CreateGradientStopCollection(&stops, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP)?;
        target.CreateLinearGradientBrush(
            &D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES { startPoint: pt(0.0, y0), endPoint: pt(0.0, y1) },
            None,
            &collection,
        )
    }

    /// Frosted-glass look: translucent rounded panel, soft top highlight and a hairline border.
    /// (True background blur is not available to per-pixel-alpha layered windows.)
    unsafe fn draw_panel(&self, target: &ID2D1DCRenderTarget, w: f32, h: f32, scale: f32, pal: &Palette, opacity: f32) -> Result<()> {
        let ph = (h * 0.80).min(h - 2.0 * scale);
        let top = (h - ph) / 2.0;
        let r = ph * 0.32;
        let shape = rounded(scale, top, w - scale, top + ph, r);
        let fill = target.CreateSolidColorBrush(&color(pal.panel, opacity.clamp(0.0, 1.0)), None)?;
        target.FillRoundedRectangle(&shape, &fill);
        let sheen = self.vertical_gradient(target, top, top + ph, &[(0.0, 0xFFFFFF, 0.10), (0.5, 0xFFFFFF, 0.02), (1.0, 0xFFFFFF, 0.0)])?;
        target.FillRoundedRectangle(&shape, &sheen);
        let border = self.gradient(target, 0.0, w, &[(0.0, pal.gradient[0], 0.35), (0.5, 0xFFFFFF, 0.12), (1.0, pal.gradient[2], 0.35)])?;
        target.DrawRoundedRectangle(&rounded(scale + 0.5, top + 0.5, w - scale - 0.5, top + ph - 0.5, r), &border, scale.max(1.0), None);
        Ok(())
    }

    unsafe fn draw_sparkline(&self, target: &ID2D1DCRenderTarget, hist: &[f32], x: f32, w: f32, h: f32, scale: f32, rgb: u32) -> Result<()> {
        if hist.len() < 2 || w <= 4.0 {
            return Ok(());
        }
        let bottom = h * 0.80;
        let height = h * 0.34;
        let step = w / (hist.len() - 1) as f32;
        let p = |i: usize| pt(x + i as f32 * step, bottom - hist[i].clamp(0.0, 1.0) * height);
        let area = self.d2d.CreatePathGeometry()?;
        let sink = area.Open()?;
        sink.BeginFigure(pt(x, bottom), D2D1_FIGURE_BEGIN_FILLED);
        for i in 0..hist.len() {
            sink.AddLine(p(i));
        }
        sink.AddLine(pt(x + w, bottom));
        sink.EndFigure(D2D1_FIGURE_END_CLOSED);
        sink.Close()?;
        let fill = self.vertical_gradient(target, bottom - height, bottom, &[(0.0, rgb, 0.28), (1.0, rgb, 0.02)])?;
        target.FillGeometry(&area, &fill, None);

        let line = self.d2d.CreatePathGeometry()?;
        let sink = line.Open()?;
        sink.BeginFigure(p(0), D2D1_FIGURE_BEGIN_HOLLOW);
        for i in 1..hist.len() {
            sink.AddLine(p(i));
        }
        sink.EndFigure(D2D1_FIGURE_END_OPEN);
        sink.Close()?;
        let stroke = target.CreateSolidColorBrush(&color(rgb, 0.55), None)?;
        target.DrawGeometry(&line, &stroke, 1.0 * scale, None);
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    unsafe fn draw_media(
        &mut self,
        target: &ID2D1DCRenderTarget,
        x: f32,
        w: f32,
        h: f32,
        scale: f32,
        cfg: &Config,
        pal: &Palette,
        playing: bool,
        text: &str,
        art: Option<&Art>,
        time: f32,
    ) -> Result<()> {
        let mut cx = x;
        if let Some(art) = art {
            let size = art_size(h);
            let top = (h - size) / 2.0;
            if let Some(bmp) = self.art_bitmap(target, art) {
                let brush = target.CreateBitmapBrush(&bmp, None, None)?;
                // Centre-crop: scale so the short side fills `size`.
                let s = size / art.width.min(art.height) as f32;
                let ox = cx - (art.width as f32 * s - size) / 2.0;
                let oy = top - (art.height as f32 * s - size) / 2.0;
                brush.SetTransform(&Matrix3x2 { M11: s, M12: 0.0, M21: 0.0, M22: s, M31: ox, M32: oy });
                target.FillRoundedRectangle(&rounded(cx, top, cx + size, top + size, 4.0 * scale), &brush);
                let edge = target.CreateSolidColorBrush(&color(0xFFFFFF, 0.18), None)?;
                target.DrawRoundedRectangle(&rounded(cx, top, cx + size, top + size, 4.0 * scale), &edge, 1.0, None);
            }
            cx += size + 7.0 * scale;
        }

        let glyph = if playing { "\u{E769}" } else { "\u{E768}" }; // pause / play
        let (lay, iw, ih) = self.layout(glyph, self.icon_fmt.as_ref().unwrap())?;
        let icon = target.CreateSolidColorBrush(&color(pal.gradient[0], 1.0), None)?;
        target.DrawTextLayout(pt(cx, (h - ih) / 2.0), &lay, &icon, D2D1_DRAW_TEXT_OPTIONS_NONE);
        cx += iw + 5.0 * scale;

        let (lay, tw, th) = self.layout(text, self.label_fmt.as_ref().unwrap())?;
        let brush = target.CreateSolidColorBrush(&color(pal.text, 1.0), None)?;
        let avail = (x + w) - cx;
        let y = (h - th) / 2.0;
        if cfg.marquee && tw > avail + 0.5 {
            // Seamless loop: draw the title twice with a gap, clipped to the title area.
            self.marquee_active = true;
            let cycle = tw + MARQUEE_GAP * scale;
            // Pause 1.5 s at the start of each loop so the beginning is readable.
            let period = cycle / (MARQUEE_SPEED * scale) + 1.5;
            let t = (time % period - 1.5).max(0.0);
            let offset = t * MARQUEE_SPEED * scale;
            target.PushAxisAlignedClip(&rect(cx, 0.0, cx + avail, h), D2D1_ANTIALIAS_MODE_ALIASED);
            target.DrawTextLayout(pt(cx - offset, y), &lay, &brush, D2D1_DRAW_TEXT_OPTIONS_NONE);
            target.DrawTextLayout(pt(cx - offset + cycle, y), &lay, &brush, D2D1_DRAW_TEXT_OPTIONS_NONE);
            target.PopAxisAlignedClip();
        } else {
            target.PushAxisAlignedClip(&rect(cx, 0.0, cx + avail.max(tw), h), D2D1_ANTIALIAS_MODE_ALIASED);
            target.DrawTextLayout(pt(cx, y), &lay, &brush, D2D1_DRAW_TEXT_OPTIONS_NONE);
            target.PopAxisAlignedClip();
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    unsafe fn draw_visualizer(
        &self,
        target: &ID2D1DCRenderTarget,
        x: f32,
        w: f32,
        h: f32,
        scale: f32,
        style: VizStyle,
        pal: &Palette,
        bars: &[f32],
        peaks: Option<&[f32]>,
    ) -> Result<()> {
        if bars.is_empty() {
            return Ok(());
        }
        let [c0, c1, c2] = pal.gradient;
        let brush = self.gradient(target, x, x + w, &[(0.0, c0, 1.0), (0.5, c1, 1.0), (1.0, c2, 1.0)])?;
        let max_h = h * 0.62;
        let base = (h + max_h) / 2.0;
        let mid = h / 2.0;
        let step = VIZ_STEP * scale;
        let bw = VIZ_BAR * scale;
        let cx = |i: usize| x + i as f32 * step + bw / 2.0;

        match style {
            VizStyle::Bars => {
                for (i, v) in bars.iter().enumerate() {
                    let bh = (v * max_h).max(1.5 * scale);
                    let bx = x + i as f32 * step;
                    brush.SetOpacity(0.55 + 0.45 * v);
                    target.FillRoundedRectangle(&rounded(bx, base - bh, bx + bw, base, bw / 2.0), &brush);
                }
            }
            VizStyle::Mirror => {
                for (i, v) in bars.iter().enumerate() {
                    let half = (v * max_h / 2.0).max(0.75 * scale);
                    let bx = x + i as f32 * step;
                    brush.SetOpacity(0.6 + 0.4 * v);
                    target.FillRoundedRectangle(&rounded(bx, mid - half, bx + bw, mid, bw / 2.0), &brush);
                    brush.SetOpacity(0.25 + 0.2 * v);
                    target.FillRoundedRectangle(&rounded(bx, mid, bx + bw, mid + half * 0.8, bw / 2.0), &brush);
                }
            }
            VizStyle::Wave => {
                let p = |i: usize| pt(cx(i), base - bars[i].clamp(0.0, 1.0) * max_h);
                let build = |filled: bool| -> Result<ID2D1PathGeometry> {
                    let g = self.d2d.CreatePathGeometry()?;
                    let sink = g.Open()?;
                    if filled {
                        sink.BeginFigure(pt(cx(0), base), D2D1_FIGURE_BEGIN_FILLED);
                        sink.AddLine(p(0));
                    } else {
                        sink.BeginFigure(p(0), D2D1_FIGURE_BEGIN_HOLLOW);
                    }
                    // Smooth curve: quadratic segments through the midpoints.
                    for i in 1..bars.len() {
                        let (a, b) = (p(i - 1), p(i));
                        let m = pt((a.X + b.X) / 2.0, (a.Y + b.Y) / 2.0);
                        sink.AddQuadraticBezier(&D2D1_QUADRATIC_BEZIER_SEGMENT { point1: a, point2: m });
                    }
                    sink.AddLine(p(bars.len() - 1));
                    if filled {
                        sink.AddLine(pt(cx(bars.len() - 1), base));
                        sink.EndFigure(D2D1_FIGURE_END_CLOSED);
                    } else {
                        sink.EndFigure(D2D1_FIGURE_END_OPEN);
                    }
                    sink.Close()?;
                    Ok(g)
                };
                let area = build(true)?;
                let line = build(false)?;
                brush.SetOpacity(0.28);
                target.FillGeometry(&area, &brush, None);
                brush.SetOpacity(1.0);
                target.DrawGeometry(&line, &brush, 1.8 * scale, None);
            }
            VizStyle::Dots => {
                let pitch = bw * 1.45;
                let levels = ((max_h / pitch).floor() as usize).max(3);
                for (i, v) in bars.iter().enumerate() {
                    let lit = ((v * levels as f32).round() as usize).max(1).min(levels);
                    for l in 0..levels {
                        let cy = base - bw / 2.0 - l as f32 * pitch;
                        brush.SetOpacity(if l < lit { 0.95 } else { 0.12 });
                        target.FillEllipse(
                            &D2D1_ELLIPSE { point: pt(cx(i), cy), radiusX: bw / 2.0, radiusY: bw / 2.0 },
                            &brush,
                        );
                    }
                }
            }
        }

        if let Some(peaks) = peaks {
            let marker = target.CreateSolidColorBrush(&color(pal.text, 0.85), None)?;
            for (i, pk) in peaks.iter().enumerate().take(bars.len()) {
                if *pk < 0.02 {
                    continue;
                }
                let bx = x + i as f32 * step;
                let y = match style {
                    VizStyle::Mirror => mid - pk * max_h / 2.0 - 2.0 * scale,
                    _ => base - pk * max_h - 2.5 * scale,
                };
                target.FillRoundedRectangle(&rounded(bx, y, bx + bw, y + 1.6 * scale, 0.8 * scale), &marker);
            }
        }
        brush.SetOpacity(1.0);
        Ok(())
    }
}

fn mascot_size(h: f32) -> f32 {
    (h * 0.8).round()
}

fn art_size(h: f32) -> f32 {
    (h * 0.64).round()
}

impl Drop for Renderer {
    fn drop(&mut self) {
        self.free_dib();
    }
}

fn pct(v: f32) -> String {
    format!("{:.0}%", v)
}

fn rate(bytes: f64) -> String {
    let kb = bytes / 1024.0;
    if kb < 1000.0 {
        format!("{:.0}K", kb)
    } else {
        format!("{:.1}M", kb / 1024.0)
    }
}

/// Normalises a history to 0..1 by a fixed full scale, or by its own maximum when `full` is None.
fn normalise(hist: Option<&std::collections::VecDeque<f32>>, full: Option<f32>) -> Option<Vec<f32>> {
    let h = hist?;
    if h.len() < 2 {
        return None;
    }
    let max = full.unwrap_or_else(|| h.iter().cloned().fold(0.0, f32::max).max(1e-3));
    Some(h.iter().map(|v| v / max).collect())
}

/// Turns the latest data into display segments, honouring the user's item selection.
pub fn build_segments(
    cfg: &Config,
    visual: &Visual,
    media: Option<&MediaInfo>,
    viz: Option<Vec<f32>>,
    peaks: Option<Vec<f32>>,
    mascot: Option<Segment>,
) -> Vec<Segment> {
    let mut mascot = mascot;
    let snap = &visual.shown;
    let mut out = Vec::new();
    // Value colour: ramp by load/temperature when enabled, otherwise red only when critical.
    let tint = |t: f32, critical: bool| -> Option<u32> {
        if cfg.color_by_value {
            Some(ramp(t))
        } else if critical {
            Some(0xFF6E6E)
        } else {
            None
        }
    };
    let spark = |item: Item, full: Option<f32>| {
        if cfg.sparklines {
            normalise(visual.history.get(&item), full)
        } else {
            None
        }
    };
    let metric = |icon: Option<char>, label: &str, value: String, color: Option<u32>, history: Option<Vec<f32>>| {
        Segment::Metric { icon, label: label.to_string(), value, color, history }
    };
    let percent = |item: Item, label: &str, v: f32| {
        metric(None, label, pct(v), tint(v / 100.0, v >= 95.0), spark(item, Some(100.0)))
    };
    // 40 °C maps to green, 90 °C to red.
    let temp = |item: Item, label: &str, t: f32, critical: f32| {
        metric(None, label, format!("{:.0}°", t), tint((t - 40.0) / 50.0, t >= critical), spark(item, Some(100.0)))
    };
    for item in &cfg.items {
        match item {
            Item::Battery => {
                if let Some(b) = snap.battery {
                    // Segoe MDL2/Fluent: Battery0..10 = U+EBA0..EBAA, BatteryCharging0..10 = U+EBAB..EBB5.
                    let step = (b.percent as u32 / 10).min(10);
                    let base = if b.charging { 0xEBAB } else { 0xEBA0 };
                    let icon = char::from_u32(base + step).unwrap_or('\u{EBAA}');
                    let low = b.percent <= 15 && !b.charging;
                    out.push(metric(Some(icon), "", pct(b.percent as f32), tint(1.0 - b.percent as f32 / 100.0, low), None));
                }
            }
            Item::Devices => {
                for d in &snap.devices {
                    let c = tint(1.0 - d.percent as f32 / 100.0, d.percent <= 15);
                    out.push(metric(Some(battery_kind_glyph(d.kind)), "", pct(d.percent as f32), c, None));
                }
            }
            Item::CpuTemp => {
                if let Some(t) = snap.cpu_temp {
                    out.push(temp(Item::CpuTemp, "CPU", t, 90.0));
                }
            }
            Item::GpuTemp => {
                if let Some(t) = snap.gpu_temp {
                    out.push(temp(Item::GpuTemp, "GPU", t, 85.0));
                }
            }
            Item::Cpu => {
                if let Some(v) = snap.cpu {
                    out.push(percent(Item::Cpu, "CPU", v));
                }
            }
            Item::PCores => {
                if let Some(v) = snap.p_cores {
                    out.push(percent(Item::PCores, "P-CORE", v));
                }
            }
            Item::ECores => {
                if let Some(v) = snap.e_cores {
                    out.push(percent(Item::ECores, "E-CORE", v));
                }
            }
            Item::Gpu => {
                if let Some(v) = snap.gpu {
                    out.push(percent(Item::Gpu, "GPU", v));
                }
            }
            Item::Vram => {
                if let Some(v) = snap.vram_used_gb {
                    out.push(metric(None, "VRAM", format!("{:.1}G", v), None, spark(Item::Vram, None)));
                }
            }
            Item::Memory => {
                if let Some(v) = snap.mem {
                    out.push(percent(Item::Memory, "MEM", v));
                }
            }
            Item::Disk => {
                if let Some(v) = snap.disk {
                    out.push(percent(Item::Disk, "DISK", v));
                }
            }
            Item::Network => {
                if let (Some(d), Some(u)) = (snap.net_down, snap.net_up) {
                    out.push(metric(None, "↓", rate(d), None, spark(Item::Network, None)));
                    out.push(metric(None, "↑", rate(u), None, None));
                }
            }
            Item::Media => {
                if let Some(m) = media {
                    let text = if m.artist.is_empty() {
                        m.title.clone()
                    } else {
                        format!("{} - {}", m.title, m.artist)
                    };
                    let art = if cfg.album_art { m.art.clone() } else { None };
                    out.push(Segment::Media { playing: m.playing, text, art });
                }
            }
            Item::Visualizer => {
                if let Some(bars) = &viz {
                    out.push(Segment::Visualizer { bars: bars.clone(), peaks: peaks.clone() });
                }
            }
            Item::Mascot => {
                if let Some(m) = mascot.take() {
                    out.push(m);
                }
            }
        }
    }
    out
}

/// Updates an embedded (child) layered window's pixels and size; its position is set
/// separately in the parent's client coordinates.
pub fn present_child(hwnd: HWND, dc: HDC, w: i32, h: i32) {
    unsafe {
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: 255,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        let size = SIZE { cx: w, cy: h };
        let src = POINT { x: 0, y: 0 };
        let _ = UpdateLayeredWindow(hwnd, None, None, Some(&size), Some(dc), Some(&src), COLORREF(0), Some(&blend), ULW_ALPHA);
    }
}

/// Pushes the rendered DIB to the layered window at the given screen position.
pub fn present(hwnd: HWND, dc: HDC, x: i32, y: i32, w: i32, h: i32) {
    unsafe {
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: 255,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        let pos = POINT { x, y };
        let size = SIZE { cx: w, cy: h };
        let src = POINT { x: 0, y: 0 };
        let _ = UpdateLayeredWindow(hwnd, None, Some(&pos), Some(&size), Some(dc), Some(&src), COLORREF(0), Some(&blend), ULW_ALPHA);
    }
}
