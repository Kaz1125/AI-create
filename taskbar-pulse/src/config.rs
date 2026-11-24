//! User settings, stored as JSON in %APPDATA%\TaskbarPulse\config.json.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Every item that can be shown on the taskbar, in display order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Item {
    Battery,
    Devices,
    CpuTemp,
    GpuTemp,
    Cpu,
    PCores,
    ECores,
    Gpu,
    Vram,
    Memory,
    Disk,
    Network,
    Media,
    Visualizer,
    Mascot,
}

impl Item {
    pub const ALL: [Item; 15] = [
        Item::Battery,
        Item::Devices,
        Item::CpuTemp,
        Item::GpuTemp,
        Item::Cpu,
        Item::PCores,
        Item::ECores,
        Item::Gpu,
        Item::Vram,
        Item::Memory,
        Item::Disk,
        Item::Network,
        Item::Media,
        Item::Visualizer,
        Item::Mascot,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Item::Battery => "バッテリー (PC)",
            Item::Devices => "マウス・コントローラーのバッテリー",
            Item::CpuTemp => "CPU 温度",
            Item::GpuTemp => "GPU 温度",
            Item::Cpu => "CPU 使用率 (全体)",
            Item::PCores => "P-CORE 使用率",
            Item::ECores => "E-CORE 使用率",
            Item::Gpu => "GPU 使用率",
            Item::Vram => "VRAM 使用量",
            Item::Memory => "メモリ使用率",
            Item::Disk => "ディスク使用率",
            Item::Network => "ネットワーク速度",
            Item::Media => "再生中のメディア",
            Item::Visualizer => "オーディオビジュアライザー",
            Item::Mascot => "ミニキャラ / GIF",
        }
    }
}

/// Performance profile. Higher profiles refresh faster and render on the GPU.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Mode {
    /// For very low-spec PCs: 5 s refresh, CPU rendering, no visualizer.
    UltraEco,
    /// 2 s refresh, CPU rendering, no visualizer.
    Eco,
    /// 1 s refresh, GPU rendering, visualizer at 30 fps.
    Standard,
    /// 0.5 s refresh, GPU rendering, visualizer at 60 fps.
    Performance,
}

impl Mode {
    pub const ALL: [Mode; 4] = [Mode::UltraEco, Mode::Eco, Mode::Standard, Mode::Performance];

    pub fn label(self) -> &'static str {
        match self {
            Mode::UltraEco => "超低負荷 (5秒更新・CPU描画)",
            Mode::Eco => "低負荷 (2秒更新・CPU描画)",
            Mode::Standard => "標準 (1秒更新・GPU描画・30fps)",
            Mode::Performance => "高負荷 (0.5秒更新・GPU描画・60fps)",
        }
    }

    /// Interval between sensor reads, in milliseconds.
    pub fn sensor_interval_ms(self) -> u32 {
        match self {
            Mode::UltraEco => 5000,
            Mode::Eco => 2000,
            Mode::Standard => 1000,
            Mode::Performance => 500,
        }
    }

    /// Visualizer frame interval in ms, or None when the visualizer is disabled.
    pub fn visualizer_interval_ms(self) -> Option<u32> {
        match self {
            Mode::UltraEco | Mode::Eco => None,
            Mode::Standard => Some(33),
            Mode::Performance => Some(16),
        }
    }

    /// Whether Direct2D should use the GPU (hardware) render target.
    pub fn hardware_rendering(self) -> bool {
        matches!(self, Mode::Standard | Mode::Performance)
    }
}

/// How strongly the visualizer reacts to the audio level.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Sensitivity {
    /// Fixed scale; quiet audio gives small bars.
    Normal,
    /// Fixed scale shifted ~25 dB for quiet playback.
    High,
    /// Automatic gain: adapts to the current loudness.
    Auto,
}

impl Sensitivity {
    pub const ALL: [Sensitivity; 3] = [Sensitivity::Auto, Sensitivity::High, Sensitivity::Normal];

    pub fn label(self) -> &'static str {
        match self {
            Sensitivity::Normal => "標準",
            Sensitivity::High => "高感度 (小さい音でも反応)",
            Sensitivity::Auto => "自動 (音量に合わせて自動調整)",
        }
    }

    pub fn code(self) -> u8 {
        match self {
            Sensitivity::Normal => 0,
            Sensitivity::High => 1,
            Sensitivity::Auto => 2,
        }
    }
}

/// Colour theme. `Custom` uses the colours stored in the config.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Theme {
    Aurora,
    Sunset,
    Neon,
    Ocean,
    Sakura,
    Mono,
    Custom,
}

/// Resolved colours used for drawing, all 0xRRGGBB.
#[derive(Clone, Copy, Debug)]
pub struct Palette {
    pub text: u32,
    pub label: u32,
    /// Visualizer / accent gradient: low pitch -> mid -> high pitch.
    pub gradient: [u32; 3],
    pub panel: u32,
}

impl Theme {
    pub const ALL: [Theme; 7] =
        [Theme::Aurora, Theme::Sunset, Theme::Neon, Theme::Ocean, Theme::Sakura, Theme::Mono, Theme::Custom];

    pub fn label(self) -> &'static str {
        match self {
            Theme::Aurora => "オーロラ (シアン→紫→ピンク)",
            Theme::Sunset => "サンセット (オレンジ→ピンク→紫)",
            Theme::Neon => "ネオン (緑→シアン→青)",
            Theme::Ocean => "オーシャン (青→水色→白)",
            Theme::Sakura => "サクラ (ピンク→薄紫→白)",
            Theme::Mono => "モノクロ",
            Theme::Custom => "カスタム (自分で選んだ色)",
        }
    }

    pub fn palette(self, cfg: &Config) -> Palette {
        let p = |text, label, gradient, panel| Palette { text, label, gradient, panel };
        match self {
            Theme::Aurora => p(0xFFFFFF, 0xB8C4D6, [0x3DDCFF, 0x8B5CF6, 0xFF4FB8], 0x10131C),
            Theme::Sunset => p(0xFFF7F0, 0xE0C2B4, [0xFFA53D, 0xFF4F8B, 0x9B4DFF], 0x1C1214),
            Theme::Neon => p(0xF0FFF6, 0xA8D8C0, [0x39FF88, 0x22D3EE, 0x3B82F6], 0x0E1714),
            Theme::Ocean => p(0xF2FAFF, 0xA9C3D9, [0x1E6BFF, 0x38BDF8, 0xE0F7FF], 0x0D1420),
            Theme::Sakura => p(0xFFF5FA, 0xE3BFD2, [0xFF7EB6, 0xC4A1FF, 0xFFFFFF], 0x1E1419),
            Theme::Mono => p(0xFFFFFF, 0xA0A0A0, [0x707070, 0xC8C8C8, 0xFFFFFF], 0x141414),
            Theme::Custom => p(
                cfg.text_color,
                cfg.label_color,
                [cfg.accent_color, cfg.accent_color_mid, cfg.accent_color_end],
                cfg.panel_color,
            ),
        }
    }
}

/// What the mascot slot shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Character {
    /// Squishy bouncing blob.
    Slime,
    /// Cat face that bobs, blinks and twitches its ears.
    Cat,
    /// Floating ghost with a wavy hem.
    Ghost,
    /// The image file in `mascot_image`.
    Image,
}

impl Character {
    pub const BUILT_IN: [Character; 3] = [Character::Slime, Character::Cat, Character::Ghost];

    pub fn label(self) -> &'static str {
        match self {
            Character::Slime => "ぷにスライム",
            Character::Cat => "ネコ",
            Character::Ghost => "おばけ",
            Character::Image => "画像 / GIF",
        }
    }
}

/// Shape of the audio visualizer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum VizStyle {
    Bars,
    Mirror,
    Wave,
    Dots,
}

impl VizStyle {
    pub const ALL: [VizStyle; 4] = [VizStyle::Bars, VizStyle::Mirror, VizStyle::Wave, VizStyle::Dots];

    pub fn label(self) -> &'static str {
        match self {
            VizStyle::Bars => "バー",
            VizStyle::Mirror => "ミラー (上下対称)",
            VizStyle::Wave => "波形ライン",
            VizStyle::Dots => "ドット",
        }
    }
}

/// Which colour the user is editing from the colour picker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColorSlot {
    Text,
    Label,
    GradientStart,
    GradientMid,
    GradientEnd,
    Panel,
}

impl ColorSlot {
    pub const ALL: [ColorSlot; 6] = [
        ColorSlot::Text,
        ColorSlot::Label,
        ColorSlot::GradientStart,
        ColorSlot::GradientMid,
        ColorSlot::GradientEnd,
        ColorSlot::Panel,
    ];

    pub fn label(self) -> &'static str {
        match self {
            ColorSlot::Text => "数値の文字色…",
            ColorSlot::Label => "ラベルの文字色…",
            ColorSlot::GradientStart => "グラデーション 開始色 (低音側)…",
            ColorSlot::GradientMid => "グラデーション 中間色…",
            ColorSlot::GradientEnd => "グラデーション 終了色 (高音側)…",
            ColorSlot::Panel => "背景パネルの色…",
        }
    }
}

/// Rasterizer choice in the advanced options.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RenderChoice {
    /// Follow the mode (GPU in Standard/High, CPU in the low-load modes).
    Auto,
    Cpu,
    Gpu,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Anchor {
    /// Right edge of the overlay sits just left of the notification area.
    BesideTray,
    /// Left edge of the taskbar.
    Left,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub items: Vec<Item>,
    pub mode: Mode,
    /// Advanced: sensor interval override in ms (100-60000). None = follow the mode.
    pub sensor_interval_override_ms: Option<u32>,
    /// Advanced: animation frame rate override (0 = no animation, max 240). None = follow the mode.
    pub fps_override: Option<u32>,
    /// Advanced: CPU or GPU drawing.
    pub render: RenderChoice,
    pub anchor: Anchor,
    /// Embed the strip into the taskbar as a child window (no z-order flicker).
    /// Off = floating always-on-top window over the taskbar.
    pub embed: bool,
    /// Extra horizontal offset in pixels (positive moves left for BesideTray, right for Left).
    pub offset_x: i32,
    /// Font size in DIPs (scaled by monitor DPI).
    pub font_size: f32,
    /// Hide the overlay while a fullscreen app or game is in front.
    pub hide_in_fullscreen: bool,
    /// Width of the media title area in DIPs; longer titles scroll (marquee).
    pub media_width: f32,
    /// Scroll long media titles instead of cutting them off.
    pub marquee: bool,
    /// Show the album art / video thumbnail next to the title.
    pub album_art: bool,
    /// Number of visualizer bars.
    pub visualizer_bars: usize,
    /// Visualizer sensitivity.
    pub visualizer_sensitivity: Sensitivity,
    pub visualizer_style: VizStyle,
    /// Small markers that hold each bar's recent peak and fall slowly.
    pub peak_hold: bool,
    /// Thin 60-second history graph behind each value.
    pub sparklines: bool,
    /// Colour values from green to yellow to red as they rise.
    pub color_by_value: bool,
    /// Numbers glide to new values (only in modes with animation).
    pub smooth_numbers: bool,
    /// Rounded translucent panel behind the strip.
    pub panel: bool,
    /// Panel opacity 0..1.
    pub panel_opacity: f32,
    pub theme: Theme,
    pub mascot: Character,
    /// Image shown when `mascot` is Image (GIF, PNG, JPEG, WebP...).
    pub mascot_image: Option<String>,
    /// Mascot moves faster as CPU usage rises.
    pub mascot_cpu_speed: bool,
    /// Monitor to show on, by device name (e.g. \\.\DISPLAY2). None = primary.
    pub monitor: Option<String>,
    /// Text colour as 0xRRGGBB.
    pub text_color: u32,
    /// Label colour as 0xRRGGBB.
    pub label_color: u32,
    /// Custom theme: gradient start colour as 0xRRGGBB.
    pub accent_color: u32,
    /// Custom theme: gradient middle colour.
    pub accent_color_mid: u32,
    /// Custom theme: gradient end colour.
    pub accent_color_end: u32,
    /// Custom theme: panel colour.
    pub panel_color: u32,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            items: vec![
                Item::Devices,
                Item::CpuTemp,
                Item::GpuTemp,
                Item::PCores,
                Item::ECores,
                Item::Gpu,
                Item::Memory,
                Item::Media,
                Item::Visualizer,
            ],
            mode: Mode::Standard,
            sensor_interval_override_ms: None,
            fps_override: None,
            render: RenderChoice::Auto,
            anchor: Anchor::BesideTray,
            embed: true,
            offset_x: 8,
            font_size: 13.0,
            hide_in_fullscreen: true,
            media_width: 180.0,
            marquee: true,
            album_art: true,
            visualizer_bars: 16,
            visualizer_sensitivity: Sensitivity::Auto,
            visualizer_style: VizStyle::Bars,
            peak_hold: true,
            sparklines: true,
            color_by_value: true,
            smooth_numbers: true,
            panel: true,
            panel_opacity: 0.55,
            theme: Theme::Aurora,
            mascot: Character::Slime,
            mascot_image: None,
            mascot_cpu_speed: true,
            monitor: None,
            text_color: 0xFFFFFF,
            label_color: 0xC8C8C8,
            accent_color: 0x3DDCFF,
            accent_color_mid: 0x8B5CF6,
            accent_color_end: 0xFF4FB8,
            panel_color: 0x10131C,
        }
    }
}

impl Config {
    pub fn dir() -> PathBuf {
        let base = std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        base.join("TaskbarPulse")
    }

    pub fn path() -> PathBuf {
        Self::dir().join("config.json")
    }

    pub fn load() -> Config {
        match std::fs::read_to_string(Self::path()) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
            Err(_) => Config::default(),
        }
    }

    pub fn save(&self) {
        let _ = std::fs::create_dir_all(Self::dir());
        if let Ok(text) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(Self::path(), text);
        }
    }

    /// Effective sensor interval (advanced override, else the mode's value).
    pub fn sensor_ms(&self) -> u32 {
        self.sensor_interval_override_ms
            .map(|v| v.clamp(100, 60_000))
            .unwrap_or_else(|| self.mode.sensor_interval_ms())
    }

    /// Effective animation frame interval in ms; None = no animation.
    pub fn frame_ms(&self) -> Option<u32> {
        match self.fps_override {
            Some(0) => None,
            Some(fps) => Some((1000 / fps.clamp(1, 240)).max(1)),
            None => self.mode.visualizer_interval_ms(),
        }
    }

    /// Effective frame rate for display in the menu.
    pub fn fps(&self) -> u32 {
        self.frame_ms().map(|ms| (1000 + ms / 2) / ms).unwrap_or(0)
    }

    pub fn hardware(&self) -> bool {
        match self.render {
            RenderChoice::Auto => self.mode.hardware_rendering(),
            RenderChoice::Cpu => false,
            RenderChoice::Gpu => true,
        }
    }

    pub fn animated(&self) -> bool {
        self.frame_ms().is_some()
    }

    pub fn palette(&self) -> Palette {
        self.theme.palette(self)
    }

    pub fn color_slot(&self, slot: ColorSlot) -> u32 {
        let p = self.palette();
        match slot {
            ColorSlot::Text => p.text,
            ColorSlot::Label => p.label,
            ColorSlot::GradientStart => p.gradient[0],
            ColorSlot::GradientMid => p.gradient[1],
            ColorSlot::GradientEnd => p.gradient[2],
            ColorSlot::Panel => p.panel,
        }
    }

    /// Sets one colour. The current theme's colours are copied into the custom
    /// slots first, so editing one colour keeps the rest of the look.
    pub fn set_color(&mut self, slot: ColorSlot, rgb: u32) {
        if self.theme != Theme::Custom {
            let p = self.palette();
            self.text_color = p.text;
            self.label_color = p.label;
            self.accent_color = p.gradient[0];
            self.accent_color_mid = p.gradient[1];
            self.accent_color_end = p.gradient[2];
            self.panel_color = p.panel;
            self.theme = Theme::Custom;
        }
        match slot {
            ColorSlot::Text => self.text_color = rgb,
            ColorSlot::Label => self.label_color = rgb,
            ColorSlot::GradientStart => self.accent_color = rgb,
            ColorSlot::GradientMid => self.accent_color_mid = rgb,
            ColorSlot::GradientEnd => self.accent_color_end = rgb,
            ColorSlot::Panel => self.panel_color = rgb,
        }
    }

    pub fn has(&self, item: Item) -> bool {
        self.items.contains(&item)
    }

    /// Toggle an item while keeping the canonical display order.
    pub fn toggle(&mut self, item: Item) {
        if self.has(item) {
            self.items.retain(|i| *i != item);
        } else {
            self.items.push(item);
            self.items
                .sort_by_key(|i| Item::ALL.iter().position(|a| a == i).unwrap_or(usize::MAX));
        }
    }
}
