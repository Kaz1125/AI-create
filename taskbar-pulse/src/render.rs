//! Draws the overlay with Direct2D + DirectWrite into a 32-bit DIB, then hands it to
//! UpdateLayeredWindow for per-pixel transparency over the taskbar.
//!
//! Low-load modes use Direct2D's software (CPU) rasterizer; Standard/Performance modes ask
//! for a hardware (GPU) render target and fall back to software if that fails.

use windows::core::{w, Result, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::battery_kind_glyph;
use crate::config::{Config, Item};
use crate::media::MediaInfo;
use crate::sensors::Snapshot;

/// One horizontal piece of the overlay.
pub enum Segment {
    /// Small grey label followed by a bold value, e.g. "CPU" "32°".
    Metric { icon: Option<char>, label: String, value: String, warn: bool },
    Media { playing: bool, text: String },
    Visualizer(Vec<f32>),
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
    fmt_key: (u32, u32),
    dib: Option<(HDC, HBITMAP, HGDIOBJ, i32, i32)>,
    /// Horizontal range of the media segment in window pixels (for click handling).
    pub media_hit: Option<(f32, f32)>,
}

fn color(rgb: u32, a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: ((rgb >> 16) & 0xFF) as f32 / 255.0,
        g: ((rgb >> 8) & 0xFF) as f32 / 255.0,
        b: (rgb & 0xFF) as f32 / 255.0,
        a,
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

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
                fmt_key: (0, 0),
                dib: None,
                media_hit: None,
            })
        }
    }

    /// Switches between GPU and CPU rasterization (takes effect on the next frame).
    pub fn set_hardware(&mut self, hardware: bool) {
        if self.hardware != hardware {
            self.hardware = hardware;
            self.target = None;
        }
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
        let key = (size.to_bits(), 1);
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
            let layout = self.dwrite.CreateTextLayout(&wide(text), fmt, 4096.0, 256.0)?;
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

    /// Measures the segments and returns the total width in pixels.
    fn measure(&mut self, segs: &[Segment], scale: f32, cfg: &Config) -> Result<Vec<f32>> {
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
                Segment::Media { text, .. } => {
                    self.layout("\u{E768}", self.icon_fmt.as_ref().unwrap())?.1
                        + 5.0 * scale
                        + self.layout(text, self.label_fmt.as_ref().unwrap())?.1
                }
                Segment::Visualizer(bars) => {
                    let _ = cfg;
                    bars.len() as f32 * 4.0 * scale
                }
            };
            widths.push(w + gap);
        }
        Ok(widths)
    }

    /// Renders the segments. Returns the DIB's DC and size for UpdateLayeredWindow.
    pub fn draw(&mut self, segs: &[Segment], height: i32, scale: f32, cfg: &Config) -> Result<(HDC, i32, i32)> {
        self.ensure_formats(cfg.font_size * scale)?;
        let widths = self.measure(segs, scale, cfg)?;
        let total = (widths.iter().sum::<f32>() + 6.0 * scale).ceil().max(8.0) as i32;
        let dc = self.ensure_dib(total, height)?;
        if self.target.is_none() {
            self.target = Some(self.make_target()?);
        }
        let target = self.target.clone().unwrap();
        let h = height as f32;
        unsafe {
            target.BindDC(dc, &RECT { left: 0, top: 0, right: total, bottom: height })?;
            target.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
            target.BeginDraw();
            // Alpha 1/255 keeps the whole strip clickable while looking fully transparent.
            target.Clear(Some(&D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 1.0 / 255.0 }));
            let text = target.CreateSolidColorBrush(&color(cfg.text_color, 1.0), None)?;
            let label = target.CreateSolidColorBrush(&color(cfg.label_color, 1.0), None)?;
            let warn = target.CreateSolidColorBrush(&color(0xFF6E6E, 1.0), None)?;
            let accent = target.CreateSolidColorBrush(&color(cfg.accent_color, 1.0), None)?;

            let mut x = 3.0 * scale;
            self.media_hit = None;
            for (seg, w) in segs.iter().zip(&widths) {
                match seg {
                    Segment::Metric { icon, label: l, value, warn: is_warn } => {
                        let mut cx = x;
                        if let Some(c) = icon {
                            let (lay, iw, ih) = self.layout(&c.to_string(), self.icon_fmt.as_ref().unwrap())?;
                            target.DrawTextLayout(pt(cx, (h - ih) / 2.0), &lay, &text, D2D1_DRAW_TEXT_OPTIONS_NONE);
                            cx += iw + 4.0 * scale;
                        }
                        if !l.is_empty() {
                            let (lay, lw, lh) = self.layout(l, self.label_fmt.as_ref().unwrap())?;
                            target.DrawTextLayout(pt(cx, (h - lh) / 2.0 + scale), &lay, &label, D2D1_DRAW_TEXT_OPTIONS_NONE);
                            cx += lw + 4.0 * scale;
                        }
                        let (lay, _, vh) = self.layout(value, self.value_fmt.as_ref().unwrap())?;
                        let brush = if *is_warn { &warn } else { &text };
                        target.DrawTextLayout(pt(cx, (h - vh) / 2.0), &lay, brush, D2D1_DRAW_TEXT_OPTIONS_NONE);
                    }
                    Segment::Media { playing, text: t } => {
                        let glyph = if *playing { "\u{E769}" } else { "\u{E768}" }; // pause / play
                        let (lay, iw, ih) = self.layout(glyph, self.icon_fmt.as_ref().unwrap())?;
                        target.DrawTextLayout(pt(x, (h - ih) / 2.0), &lay, &accent, D2D1_DRAW_TEXT_OPTIONS_NONE);
                        let (lay, _, th) = self.layout(t, self.label_fmt.as_ref().unwrap())?;
                        target.DrawTextLayout(
                            pt(x + iw + 5.0 * scale, (h - th) / 2.0),
                            &lay,
                            &text,
                            D2D1_DRAW_TEXT_OPTIONS_NONE,
                        );
                        self.media_hit = Some((x, x + w));
                    }
                    Segment::Visualizer(bars) => {
                        let max_h = h * 0.62;
                        let base = (h + max_h) / 2.0;
                        let bw = 3.0 * scale;
                        for (i, v) in bars.iter().enumerate() {
                            let bh = (v * max_h).max(1.5 * scale);
                            let bx = x + i as f32 * 4.0 * scale;
                            accent.SetOpacity(0.45 + 0.55 * v);
                            target.FillRoundedRectangle(
                                &D2D1_ROUNDED_RECT {
                                    rect: D2D_RECT_F { left: bx, top: base - bh, right: bx + bw, bottom: base },
                                    radiusX: bw / 2.0,
                                    radiusY: bw / 2.0,
                                },
                                &accent,
                            );
                        }
                        accent.SetOpacity(1.0);
                    }
                }
                x += w;
            }
            if let Err(e) = target.EndDraw(None, None) {
                // Device lost (e.g. GPU driver reset): recreate next frame.
                self.target = None;
                return Err(e);
            }
        }
        Ok((dc, total, height))
    }
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

fn truncate(s: &str, max: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max {
        s.to_string()
    } else {
        chars[..max.saturating_sub(1)].iter().collect::<String>() + "…"
    }
}

/// Turns the latest data into display segments, honouring the user's item selection.
pub fn build_segments(cfg: &Config, snap: &Snapshot, media: Option<&MediaInfo>, viz: Option<Vec<f32>>) -> Vec<Segment> {
    let mut out = Vec::new();
    let metric = |icon: Option<char>, label: &str, value: String, warn: bool| Segment::Metric {
        icon,
        label: label.to_string(),
        value,
        warn,
    };
    for item in &cfg.items {
        match item {
            Item::Battery => {
                if let Some(b) = snap.battery {
                    // Segoe MDL2/Fluent: Battery0..10 = U+EBA0..EBAA, BatteryCharging0..10 = U+EBAB..EBB5.
                    let step = (b.percent as u32 / 10).min(10);
                    let base = if b.charging { 0xEBAB } else { 0xEBA0 };
                    let icon = char::from_u32(base + step).unwrap_or('\u{EBAA}');
                    out.push(metric(Some(icon), "", pct(b.percent as f32), b.percent <= 15 && !b.charging));
                }
            }
            Item::Devices => {
                for d in &snap.devices {
                    out.push(metric(Some(battery_kind_glyph(d.kind)), "", pct(d.percent as f32), d.percent <= 15));
                }
            }
            Item::CpuTemp => {
                if let Some(t) = snap.cpu_temp {
                    out.push(metric(None, "CPU", format!("{:.0}°", t), t >= 90.0));
                }
            }
            Item::GpuTemp => {
                if let Some(t) = snap.gpu_temp {
                    out.push(metric(None, "GPU", format!("{:.0}°", t), t >= 85.0));
                }
            }
            Item::Cpu => {
                if let Some(v) = snap.cpu {
                    out.push(metric(None, "CPU", pct(v), v >= 95.0));
                }
            }
            Item::PCores => {
                if let Some(v) = snap.p_cores {
                    out.push(metric(None, "P-CORE", pct(v), v >= 95.0));
                }
            }
            Item::ECores => {
                if let Some(v) = snap.e_cores {
                    out.push(metric(None, "E-CORE", pct(v), v >= 95.0));
                }
            }
            Item::Gpu => {
                if let Some(v) = snap.gpu {
                    out.push(metric(None, "GPU", pct(v), v >= 95.0));
                }
            }
            Item::Vram => {
                if let Some(v) = snap.vram_used_gb {
                    out.push(metric(None, "VRAM", format!("{:.1}G", v), false));
                }
            }
            Item::Memory => {
                if let Some(v) = snap.mem {
                    out.push(metric(None, "MEM", pct(v), v >= 90.0));
                }
            }
            Item::Disk => {
                if let Some(v) = snap.disk {
                    out.push(metric(None, "DISK", pct(v), v >= 95.0));
                }
            }
            Item::Network => {
                if let (Some(d), Some(u)) = (snap.net_down, snap.net_up) {
                    out.push(metric(None, "↓", rate(d), false));
                    out.push(metric(None, "↑", rate(u), false));
                }
            }
            Item::Media => {
                if let Some(m) = media {
                    let text = if m.artist.is_empty() {
                        m.title.clone()
                    } else {
                        format!("{} - {}", m.title, m.artist)
                    };
                    out.push(Segment::Media { playing: m.playing, text: truncate(&text, cfg.media_max_chars) });
                }
            }
            Item::Visualizer => {
                if let Some(bars) = &viz {
                    out.push(Segment::Visualizer(bars.clone()));
                }
            }
        }
    }
    out
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

fn pt(x: f32, y: f32) -> windows_numerics::Vector2 {
    windows_numerics::Vector2 { X: x, Y: y }
}
