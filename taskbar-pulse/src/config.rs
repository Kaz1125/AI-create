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
}

impl Item {
    pub const ALL: [Item; 14] = [
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
    pub anchor: Anchor,
    /// Extra horizontal offset in pixels (positive moves left for BesideTray, right for Left).
    pub offset_x: i32,
    /// Font size in DIPs (scaled by monitor DPI).
    pub font_size: f32,
    /// Hide the overlay while a fullscreen app or game is in front.
    pub hide_in_fullscreen: bool,
    /// Maximum characters shown for the media title.
    pub media_max_chars: usize,
    /// Number of visualizer bars.
    pub visualizer_bars: usize,
    /// Text colour as 0xRRGGBB.
    pub text_color: u32,
    /// Label colour as 0xRRGGBB.
    pub label_color: u32,
    /// Accent colour (visualizer, warnings) as 0xRRGGBB.
    pub accent_color: u32,
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
            anchor: Anchor::BesideTray,
            offset_x: 8,
            font_size: 13.0,
            hide_in_fullscreen: true,
            media_max_chars: 28,
            visualizer_bars: 16,
            text_color: 0xFFFFFF,
            label_color: 0xC8C8C8,
            accent_color: 0x4FC3F7,
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
