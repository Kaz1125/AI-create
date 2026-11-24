//! Notification-area icon, right-click menu and "start with Windows" setting.

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::Registry::*;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::config::{Anchor, Character, MoveTo, RenderChoice, ColorSlot, Config, Item, Mode, Sensitivity, Theme, TrackAnim, VizStyle};
use windows::Win32::UI::Controls::Dialogs::*;
use crate::WM_APP_TRAY;

const ID_ITEM: u16 = 100;
const ID_MODE: u16 = 200;
const ID_ANCHOR_TRAY: u16 = 300;
const ID_ANCHOR_LEFT: u16 = 301;
const ID_FONT: u16 = 310;
const ID_SENS: u16 = 315;
const ID_FULLSCREEN: u16 = 320;
const ID_AUTOSTART: u16 = 330;
const ID_OPEN_CONFIG: u16 = 340;
const ID_EXIT: u16 = 399;
/// Version shown to the user, e.g. "v5.2". Major = big feature, minor = small fix/feature.
pub const VERSION: &str = concat!("v", env!("CARGO_PKG_VERSION_MAJOR"), ".", env!("CARGO_PKG_VERSION_MINOR"));
const ID_THEME: u16 = 400;
const ID_STYLE: u16 = 420;
const ID_FLAG: u16 = 440;
const ID_OPACITY: u16 = 460;
const ID_COLOR: u16 = 480;
const ID_CHAR: u16 = 500;
const ID_PICK_IMAGE: u16 = 510;
const ID_MASCOT_SPEED: u16 = 511;
const ID_MONITOR_PRIMARY: u16 = 520;
const ID_MONITOR: u16 = 521;
const ID_EMBED: u16 = 530;
const ID_SENSOR: u16 = 540;
const ID_ORDER_RESET: u16 = 599;
/// 600 + index * 4 + direction (left, right, first, last).
const ID_ORDER: u16 = 600;
const MOVES: [MoveTo; 4] = [MoveTo::Left, MoveTo::Right, MoveTo::First, MoveTo::Last];
const ID_FPS: u16 = 560;
const ID_RENDER: u16 = 580;
const ID_TRACK_ANIM: u16 = 700;
/// Sensor interval presets in ms (None = follow the mode).
const SENSOR_PRESETS: [Option<u32>; 10] =
    [None, Some(250), Some(500), Some(1000), Some(1500), Some(2000), Some(3000), Some(5000), Some(10_000), Some(30_000)];
/// Frame rate presets (None = follow the mode, 0 = no animation).
const FPS_PRESETS: [Option<u32>; 13] =
    [None, Some(0), Some(5), Some(10), Some(15), Some(20), Some(24), Some(30), Some(45), Some(60), Some(90), Some(120), Some(144)];
const RENDERS: [RenderChoice; 3] = [RenderChoice::Auto, RenderChoice::Cpu, RenderChoice::Gpu];
const OPACITIES: [(f32, &str); 3] = [(0.35, "薄い"), (0.55, "普通"), (0.8, "濃い")];

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Flag {
    PeakHold,
    Sparklines,
    ColorByValue,
    SmoothNumbers,
    Panel,
    AlbumArt,
    Marquee,
}

const FLAGS: [Flag; 7] = [
    Flag::PeakHold,
    Flag::Sparklines,
    Flag::ColorByValue,
    Flag::SmoothNumbers,
    Flag::Panel,
    Flag::AlbumArt,
    Flag::Marquee,
];

impl Flag {
    fn label(self) -> &'static str {
        match self {
            Flag::PeakHold => "ピークホールド",
            Flag::Sparklines => "ミニ履歴グラフ (約60秒)",
            Flag::ColorByValue => "値に応じて色を変える (緑→黄→赤)",
            Flag::SmoothNumbers => "数値をなめらかに変化",
            Flag::Panel => "背景パネル (すりガラス風)",
            Flag::AlbumArt => "アルバムアートを表示",
            Flag::Marquee => "長い曲名をスクロール",
        }
    }

    fn get(self, c: &Config) -> bool {
        match self {
            Flag::PeakHold => c.peak_hold,
            Flag::Sparklines => c.sparklines,
            Flag::ColorByValue => c.color_by_value,
            Flag::SmoothNumbers => c.smooth_numbers,
            Flag::Panel => c.panel,
            Flag::AlbumArt => c.album_art,
            Flag::Marquee => c.marquee,
        }
    }

    /// Effects that need animation frames, which low-load modes do not have.
    fn needs_animation(self) -> bool {
        matches!(self, Flag::PeakHold | Flag::SmoothNumbers | Flag::Marquee)
    }
}
const FONT_SIZES: [(f32, &str); 3] = [(11.0, "小"), (13.0, "中"), (15.0, "大")];

pub enum Action {
    ToggleItem(Item),
    SetMode(Mode),
    SetAnchor(Anchor),
    FontSize(f32),
    SetSensitivity(Sensitivity),
    SetTheme(Theme),
    SetVizStyle(VizStyle),
    SetTrackAnim(TrackAnim),
    Toggle(Flag),
    PanelOpacity(f32),
    PickColor(ColorSlot),
    SetCharacter(Character),
    PickImage,
    ToggleMascotSpeed,
    /// None = primary monitor.
    SetMonitor(Option<String>),
    SetSensorOverride(Option<u32>),
    ToggleEmbed,
    MoveItem(usize, MoveTo),
    ResetOrder,
    SetFpsOverride(Option<u32>),
    SetRender(RenderChoice),
    ToggleFullscreenHide,
    ToggleAutostart,
    OpenConfig,
    Exit,
    None,
}

pub fn decode(id: u16) -> Action {
    match id {
        i if (ID_ITEM..ID_ITEM + Item::ALL.len() as u16).contains(&i) => {
            Action::ToggleItem(Item::ALL[(i - ID_ITEM) as usize])
        }
        i if (ID_MODE..ID_MODE + Mode::ALL.len() as u16).contains(&i) => {
            Action::SetMode(Mode::ALL[(i - ID_MODE) as usize])
        }
        ID_ANCHOR_TRAY => Action::SetAnchor(Anchor::BesideTray),
        ID_ANCHOR_LEFT => Action::SetAnchor(Anchor::Left),
        i if (ID_FONT..ID_FONT + FONT_SIZES.len() as u16).contains(&i) => {
            Action::FontSize(FONT_SIZES[(i - ID_FONT) as usize].0)
        }
        i if (ID_SENS..ID_SENS + Sensitivity::ALL.len() as u16).contains(&i) => {
            Action::SetSensitivity(Sensitivity::ALL[(i - ID_SENS) as usize])
        }
        i if (ID_THEME..ID_THEME + Theme::ALL.len() as u16).contains(&i) => {
            Action::SetTheme(Theme::ALL[(i - ID_THEME) as usize])
        }
        i if (ID_STYLE..ID_STYLE + VizStyle::ALL.len() as u16).contains(&i) => {
            Action::SetVizStyle(VizStyle::ALL[(i - ID_STYLE) as usize])
        }
        i if (ID_TRACK_ANIM..ID_TRACK_ANIM + TrackAnim::ALL.len() as u16).contains(&i) => {
            Action::SetTrackAnim(TrackAnim::ALL[(i - ID_TRACK_ANIM) as usize])
        }
        i if (ID_FLAG..ID_FLAG + FLAGS.len() as u16).contains(&i) => Action::Toggle(FLAGS[(i - ID_FLAG) as usize]),
        i if (ID_OPACITY..ID_OPACITY + OPACITIES.len() as u16).contains(&i) => {
            Action::PanelOpacity(OPACITIES[(i - ID_OPACITY) as usize].0)
        }
        i if (ID_COLOR..ID_COLOR + ColorSlot::ALL.len() as u16).contains(&i) => {
            Action::PickColor(ColorSlot::ALL[(i - ID_COLOR) as usize])
        }
        i if (ID_CHAR..ID_CHAR + Character::BUILT_IN.len() as u16).contains(&i) => {
            Action::SetCharacter(Character::BUILT_IN[(i - ID_CHAR) as usize])
        }
        ID_PICK_IMAGE => Action::PickImage,
        ID_MASCOT_SPEED => Action::ToggleMascotSpeed,
        ID_MONITOR_PRIMARY => Action::SetMonitor(None),
        i if (ID_MONITOR..ID_MONITOR + 16).contains(&i) => match crate::monitor::list().get((i - ID_MONITOR) as usize) {
            Some(m) => Action::SetMonitor(Some(m.name.clone())),
            None => Action::None,
        },
        i if (ID_SENSOR..ID_SENSOR + SENSOR_PRESETS.len() as u16).contains(&i) => {
            Action::SetSensorOverride(SENSOR_PRESETS[(i - ID_SENSOR) as usize])
        }
        i if (ID_FPS..ID_FPS + FPS_PRESETS.len() as u16).contains(&i) => {
            Action::SetFpsOverride(FPS_PRESETS[(i - ID_FPS) as usize])
        }
        i if (ID_RENDER..ID_RENDER + RENDERS.len() as u16).contains(&i) => Action::SetRender(RENDERS[(i - ID_RENDER) as usize]),
        ID_EMBED => Action::ToggleEmbed,
        ID_ORDER_RESET => Action::ResetOrder,
        i if (ID_ORDER..ID_ORDER + 4 * Item::ALL.len() as u16).contains(&i) => {
            let n = (i - ID_ORDER) as usize;
            Action::MoveItem(n / 4, MOVES[n % 4])
        }
        ID_FULLSCREEN => Action::ToggleFullscreenHide,
        ID_AUTOSTART => Action::ToggleAutostart,
        ID_OPEN_CONFIG => Action::OpenConfig,
        ID_EXIT => Action::Exit,
        _ => Action::None,
    }
}

/// Draws a small three-bar meter icon at runtime (no resource compiler needed).
/// The app icon (resource 1, embedded by build.rs) at the small-icon size for the
/// current DPI; falls back to a drawn icon if the resource is missing.
fn app_icon() -> HICON {
    unsafe {
        let module = windows::Win32::System::LibraryLoader::GetModuleHandleW(None).ok();
        let (cx, cy) = (GetSystemMetrics(SM_CXSMICON), GetSystemMetrics(SM_CYSMICON));
        if let Some(m) = module {
            if let Ok(h) = LoadImageW(Some(m.into()), PCWSTR(1 as _), IMAGE_ICON, cx, cy, LR_DEFAULTCOLOR) {
                return HICON(h.0);
            }
        }
    }
    make_icon()
}

fn make_icon() -> HICON {
    const S: i32 = 32;
    let mut pixels = vec![0u32; (S * S) as usize];
    let bars = [(4, 12, 0xFF4FC3F7u32), (13, 6, 0xFF81C784), (22, 18, 0xFFFFB74D)];
    for (x0, top, color) in bars {
        for y in top..28 {
            for x in x0..x0 + 7 {
                pixels[(y * S + x) as usize] = color;
            }
        }
    }
    unsafe {
        let color = CreateBitmap(S, S, 1, 32, Some(pixels.as_ptr() as _));
        let mask_bits = vec![0u8; (S * S / 8) as usize];
        let mask = CreateBitmap(S, S, 1, 1, Some(mask_bits.as_ptr() as _));
        let info = ICONINFO { fIcon: TRUE, xHotspot: 0, yHotspot: 0, hbmMask: mask, hbmColor: color };
        let icon = CreateIconIndirect(&info).unwrap_or_default();
        let _ = DeleteObject(color.into());
        let _ = DeleteObject(mask.into());
        icon
    }
}

fn icon_data(hwnd: HWND) -> NOTIFYICONDATAW {
    let mut data = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: 1,
        ..Default::default()
    };
    let tip: Vec<u16> = format!("Taskbar Pulse {}", VERSION).encode_utf16().collect();
    data.szTip[..tip.len()].copy_from_slice(&tip);
    data
}

pub fn add(hwnd: HWND) {
    let mut data = icon_data(hwnd);
    data.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
    data.uCallbackMessage = WM_APP_TRAY;
    data.hIcon = app_icon();
    unsafe {
        let _ = Shell_NotifyIconW(NIM_ADD, &data);
    }
}

pub fn remove(hwnd: HWND) {
    let data = icon_data(hwnd);
    unsafe {
        let _ = Shell_NotifyIconW(NIM_DELETE, &data);
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}

unsafe fn append(menu: HMENU, flags: MENU_ITEM_FLAGS, id: usize, text: &str) {
    let t = wide(text);
    let _ = AppendMenuW(menu, flags, id, PCWSTR(t.as_ptr()));
}

fn check(on: bool) -> MENU_ITEM_FLAGS {
    if on {
        MF_CHECKED
    } else {
        MF_UNCHECKED
    }
}

pub fn show_menu(hwnd: HWND, cfg: &Config, hybrid: bool, hardware_active: bool) {
    unsafe {
        let Ok(menu) = CreatePopupMenu() else { return };

        let items = CreatePopupMenu().unwrap_or_default();
        for (i, item) in Item::ALL.iter().enumerate() {
            let mut flags = MF_STRING | check(cfg.has(*item));
            let mut label = item.label().to_string();
            if matches!(item, Item::PCores | Item::ECores) && !hybrid {
                flags |= MF_GRAYED;
                label += " (このCPUは非対応)";
            }
            if *item == Item::Visualizer && !cfg.animated() {
                label += " (標準/高負荷モードで有効)";
            }
            append(items, flags, (ID_ITEM + i as u16) as usize, &label);
        }
        append(menu, MF_POPUP, items.0 as usize, "表示項目");

        // Display order: one submenu per shown item, left to right.
        let order = CreatePopupMenu().unwrap_or_default();
        let shown = cfg.shown();
        let count = shown.len();
        for (idx, item) in shown.iter().enumerate() {
            let sub = CreatePopupMenu().unwrap_or_default();
            let base = ID_ORDER + idx as u16 * 4;
            let first = idx == 0;
            let last = idx + 1 == count;
            let gray = |off: bool| if off { MF_STRING | MF_GRAYED } else { MF_STRING };
            append(sub, gray(first), (base) as usize, "← 1つ左へ");
            append(sub, gray(last), (base + 1) as usize, "→ 1つ右へ");
            append(sub, gray(first), (base + 2) as usize, "⇤ 一番左へ");
            append(sub, gray(last), (base + 3) as usize, "⇥ 一番右へ");
            let label = format!("{}. {}", idx + 1, item.label());
            append(order, MF_POPUP, sub.0 as usize, &label);
        }
        append(order, MF_SEPARATOR, 0, "");
        append(order, MF_STRING, ID_ORDER_RESET as usize, "初期の並び順に戻す");
        append(menu, MF_POPUP, order.0 as usize, "並び順 (左→右)");

        let modes = CreatePopupMenu().unwrap_or_default();
        for (i, mode) in Mode::ALL.iter().enumerate() {
            append(modes, MF_STRING | check(cfg.mode == *mode), (ID_MODE + i as u16) as usize, mode.label());
        }
        append(modes, MF_SEPARATOR, 0, "");
        let status = if hardware_active { "現在の描画: GPU" } else { "現在の描画: CPU" };
        append(modes, MF_STRING | MF_GRAYED, 0, status);
        if cfg.sensor_interval_override_ms.is_some() || cfg.fps_override.is_some() || cfg.render != RenderChoice::Auto {
            append(modes, MF_STRING | MF_GRAYED, 0, "※ 詳細オプションの設定が優先されています");
        }
        append(menu, MF_POPUP, modes.0 as usize, "動作モード");

        // Advanced options
        let adv = CreatePopupMenu().unwrap_or_default();
        let secs = |ms: u32| {
            if ms % 1000 == 0 {
                format!("{}秒", ms / 1000)
            } else {
                format!("{}秒", ms as f32 / 1000.0)
            }
        };
        let sensor = CreatePopupMenu().unwrap_or_default();
        for (i, p) in SENSOR_PRESETS.iter().enumerate() {
            let label = match p {
                None => format!("自動 (モードに従う: {})", secs(cfg.mode.sensor_interval_ms())),
                Some(ms) => secs(*ms),
            };
            append(sensor, MF_STRING | check(cfg.sensor_interval_override_ms == *p), (ID_SENSOR + i as u16) as usize, &label);
        }
        if let Some(ms) = cfg.sensor_interval_override_ms.filter(|v| !SENSOR_PRESETS.contains(&Some(*v))) {
            append(sensor, MF_STRING | MF_GRAYED | MF_CHECKED, 0, &format!("{} (設定ファイルで指定)", secs(ms.clamp(100, 60_000))));
        }
        append(adv, MF_POPUP, sensor.0 as usize, &format!("センサー取得間隔 (現在 {})", secs(cfg.sensor_ms())));

        let fps = CreatePopupMenu().unwrap_or_default();
        for (i, p) in FPS_PRESETS.iter().enumerate() {
            let label = match p {
                None => {
                    let m = cfg.mode.visualizer_interval_ms().map(|ms| (1000 + ms / 2) / ms).unwrap_or(0);
                    format!("自動 (モードに従う: {} fps)", m)
                }
                Some(0) => "0 (アニメーションなし)".to_string(),
                Some(f) if *f > 60 => format!("{f} fps (※実際は約60fpsが上限の場合あり)"),
                Some(f) => format!("{f} fps"),
            };
            append(fps, MF_STRING | check(cfg.fps_override == *p), (ID_FPS + i as u16) as usize, &label);
        }
        if let Some(f) = cfg.fps_override.filter(|v| !FPS_PRESETS.contains(&Some(*v))) {
            append(fps, MF_STRING | MF_GRAYED | MF_CHECKED, 0, &format!("{} fps (設定ファイルで指定)", f.min(240)));
        }
        append(adv, MF_POPUP, fps.0 as usize, &format!("描画FPS (現在 {} fps)", cfg.fps()));

        let render = CreatePopupMenu().unwrap_or_default();
        for (i, r) in RENDERS.iter().enumerate() {
            let label = match r {
                RenderChoice::Auto => "自動 (モードに従う)",
                RenderChoice::Cpu => "CPU描画 (GPUを使わない)",
                RenderChoice::Gpu => "GPU描画",
            };
            append(render, MF_STRING | check(cfg.render == *r), (ID_RENDER + i as u16) as usize, label);
        }
        append(render, MF_SEPARATOR, 0, "");
        append(render, MF_STRING | MF_GRAYED, 0, if hardware_active { "現在の描画: GPU" } else { "現在の描画: CPU" });
        append(adv, MF_POPUP, render.0 as usize, "描画方式");
        append(adv, MF_SEPARATOR, 0, "");
        append(adv, MF_STRING | MF_GRAYED, 0, "細かい値は設定ファイルでも指定できます");
        append(menu, MF_POPUP, adv.0 as usize, "詳細オプション");

        // Colours
        let colors = CreatePopupMenu().unwrap_or_default();
        for (i, t) in Theme::ALL.iter().enumerate() {
            append(colors, MF_STRING | check(cfg.theme == *t), (ID_THEME + i as u16) as usize, t.label());
        }
        append(colors, MF_SEPARATOR, 0, "");
        for (i, slot) in ColorSlot::ALL.iter().enumerate() {
            append(colors, MF_STRING, (ID_COLOR + i as u16) as usize, slot.label());
        }
        append(colors, MF_SEPARATOR, 0, "");
        let flag_index = |f: Flag| FLAGS.iter().position(|x| *x == f).unwrap() as u16;
        append(
            colors,
            MF_STRING | check(cfg.color_by_value),
            (ID_FLAG + flag_index(Flag::ColorByValue)) as usize,
            Flag::ColorByValue.label(),
        );
        append(menu, MF_POPUP, colors.0 as usize, "カラー");

        // Look & effects
        let look = CreatePopupMenu().unwrap_or_default();
        let styles = CreatePopupMenu().unwrap_or_default();
        for (i, st) in VizStyle::ALL.iter().enumerate() {
            append(styles, MF_STRING | check(cfg.visualizer_style == *st), (ID_STYLE + i as u16) as usize, st.label());
        }
        append(look, MF_POPUP, styles.0 as usize, "ビジュアライザーの形");
        let animated = cfg.animated();
        let track = CreatePopupMenu().unwrap_or_default();
        for (i, a) in TrackAnim::ALL.iter().enumerate() {
            append(track, MF_STRING | check(cfg.track_anim == *a), (ID_TRACK_ANIM + i as u16) as usize, a.label());
        }
        let track_label = if animated { "曲切り替えアニメーション" } else { "曲切り替えアニメーション (標準/高負荷モードで有効)" };
        append(look, MF_POPUP, track.0 as usize, track_label);
        for f in FLAGS {
            if f == Flag::ColorByValue {
                continue;
            }
            let mut label = f.label().to_string();
            if f.needs_animation() && !animated {
                label += " (標準/高負荷モードで有効)";
            }
            append(look, MF_STRING | check(f.get(cfg)), (ID_FLAG + flag_index(f)) as usize, &label);
        }
        let opacity = CreatePopupMenu().unwrap_or_default();
        for (i, (o, name)) in OPACITIES.iter().enumerate() {
            let on = cfg.panel && (cfg.panel_opacity - o).abs() < 0.01;
            append(opacity, MF_STRING | check(on), (ID_OPACITY + i as u16) as usize, name);
        }
        append(look, MF_POPUP, opacity.0 as usize, "背景パネルの濃さ");
        append(menu, MF_POPUP, look.0 as usize, "見た目・エフェクト");

        // Mascot
        let mascot = CreatePopupMenu().unwrap_or_default();
        let shown = cfg.has(Item::Mascot);
        for (i, c) in Character::BUILT_IN.iter().enumerate() {
            append(mascot, MF_STRING | check(shown && cfg.mascot == *c), (ID_CHAR + i as u16) as usize, c.label());
        }
        let image_label = match (&cfg.mascot_image, cfg.mascot == Character::Image) {
            (Some(p), _) => {
                let name = std::path::Path::new(p).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                format!("画像 / GIF を選ぶ… (現在: {name})")
            }
            _ => "画像 / GIF を選ぶ…".to_string(),
        };
        append(mascot, MF_STRING | check(shown && cfg.mascot == Character::Image), ID_PICK_IMAGE as usize, &image_label);
        append(mascot, MF_SEPARATOR, 0, "");
        append(mascot, MF_STRING | check(cfg.mascot_cpu_speed), ID_MASCOT_SPEED as usize, "CPU負荷で動きが速くなる");
        if !cfg.animated() {
            append(mascot, MF_STRING | MF_GRAYED, 0, "※ 低負荷モードでは止まった表示になります");
        }
        append(menu, MF_POPUP, mascot.0 as usize, "ミニキャラ / GIF");

        // Monitor
        let mons = CreatePopupMenu().unwrap_or_default();
        append(mons, MF_STRING | check(cfg.monitor.is_none()), ID_MONITOR_PRIMARY as usize, "メインモニター (自動)");
        for (i, m) in crate::monitor::list().iter().enumerate().take(16) {
            let w = m.rect.right - m.rect.left;
            let h = m.rect.bottom - m.rect.top;
            let short = m.name.trim_start_matches("\\\\.\\");
            let label = format!("{}: {}  {}×{}{}", i + 1, short, w, h, if m.primary { " (メイン)" } else { "" });
            append(mons, MF_STRING | check(cfg.monitor.as_deref() == Some(m.name.as_str())), (ID_MONITOR + i as u16) as usize, &label);
        }
        append(menu, MF_POPUP, mons.0 as usize, "表示するモニター");

        let pos = CreatePopupMenu().unwrap_or_default();
        append(pos, MF_STRING | check(cfg.anchor == Anchor::BesideTray), ID_ANCHOR_TRAY as usize, "通知領域の左 (位置リセット)");
        append(pos, MF_STRING | check(cfg.anchor == Anchor::Left), ID_ANCHOR_LEFT as usize, "タスクバーの左端 (位置リセット)");
        append(pos, MF_SEPARATOR, 0, "");
        append(pos, MF_STRING | check(cfg.embed), ID_EMBED as usize, "タスクバーに埋め込む (おすすめ・ちらつき防止)");
        append(pos, MF_SEPARATOR, 0, "");
        append(pos, MF_STRING | MF_GRAYED, 0, "表示部分を左ドラッグで位置調整できます");
        append(menu, MF_POPUP, pos.0 as usize, "表示位置");

        let fonts = CreatePopupMenu().unwrap_or_default();
        for (i, (size, name)) in FONT_SIZES.iter().enumerate() {
            append(fonts, MF_STRING | check((cfg.font_size - size).abs() < 0.1), (ID_FONT + i as u16) as usize, name);
        }
        append(menu, MF_POPUP, fonts.0 as usize, "文字サイズ");

        let sens = CreatePopupMenu().unwrap_or_default();
        for (i, s) in Sensitivity::ALL.iter().enumerate() {
            append(sens, MF_STRING | check(cfg.visualizer_sensitivity == *s), (ID_SENS + i as u16) as usize, s.label());
        }
        append(menu, MF_POPUP, sens.0 as usize, "ビジュアライザー感度");

        append(menu, MF_SEPARATOR, 0, "");
        append(menu, MF_STRING | check(cfg.hide_in_fullscreen), ID_FULLSCREEN as usize, "全画面アプリ・ゲーム中は隠す");
        append(menu, MF_STRING | check(autostart_enabled()), ID_AUTOSTART as usize, "Windows起動時に開始");
        append(menu, MF_STRING, ID_OPEN_CONFIG as usize, "設定ファイルを開く");
        append(menu, MF_SEPARATOR, 0, "");
        append(menu, MF_STRING | MF_GRAYED, 0, &format!("Taskbar Pulse {}", VERSION));
        append(menu, MF_STRING, ID_EXIT as usize, "終了");

        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        // Required so the menu closes when clicking elsewhere.
        let _ = SetForegroundWindow(hwnd);
        let _ = TrackPopupMenu(menu, TPM_RIGHTBUTTON | TPM_BOTTOMALIGN, pt.x, pt.y, Some(0), hwnd, None);
        let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(menu);
    }
}

const RUN_KEY: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
const RUN_NAME: PCWSTR = w!("TaskbarPulse");

pub fn autostart_enabled() -> bool {
    unsafe { RegGetValueW(HKEY_CURRENT_USER, RUN_KEY, RUN_NAME, RRF_RT_REG_SZ, None, None, None).is_ok() }
}

pub fn set_autostart(enable: bool) {
    unsafe {
        let mut key = HKEY::default();
        if RegOpenKeyExW(HKEY_CURRENT_USER, RUN_KEY, Some(0), KEY_SET_VALUE, &mut key).is_err() {
            return;
        }
        if enable {
            if let Ok(exe) = std::env::current_exe() {
                let value = wide(&format!("\"{}\"", exe.display()));
                let bytes = std::slice::from_raw_parts(value.as_ptr() as *const u8, value.len() * 2);
                let _ = RegSetValueExW(key, RUN_NAME, Some(0), REG_SZ, Some(bytes));
            }
        } else {
            let _ = RegDeleteValueW(key, RUN_NAME);
        }
        let _ = RegCloseKey(key);
    }
}

/// Opens the standard Windows colour picker. Returns 0xRRGGBB, or None if cancelled.
pub fn pick_color(owner: HWND, current: u32) -> Option<u32> {
    // COLORREF is 0x00BBGGRR.
    let to_ref = |rgb: u32| COLORREF(((rgb & 0xFF) << 16) | (rgb & 0xFF00) | ((rgb >> 16) & 0xFF));
    let mut custom = [COLORREF(0x00FFFFFF); 16];
    let mut cc = CHOOSECOLORW {
        lStructSize: std::mem::size_of::<CHOOSECOLORW>() as u32,
        hwndOwner: owner,
        rgbResult: to_ref(current),
        lpCustColors: custom.as_mut_ptr(),
        Flags: CC_RGBINIT | CC_FULLOPEN | CC_ANYCOLOR,
        ..Default::default()
    };
    unsafe {
        if ChooseColorW(&mut cc).as_bool() {
            let c = cc.rgbResult.0;
            Some(((c & 0xFF) << 16) | (c & 0xFF00) | ((c >> 16) & 0xFF))
        } else {
            None
        }
    }
}

/// Standard "open file" dialog for the mascot image. Returns the chosen path.
pub fn pick_image(owner: HWND) -> Option<String> {
    let filter: Vec<u16> = "画像 (GIF, PNG, JPEG, WebP, BMP)\0*.gif;*.png;*.jpg;*.jpeg;*.webp;*.bmp\0すべてのファイル\0*.*\0\0"
        .encode_utf16()
        .collect();
    let mut file = vec![0u16; 1024];
    let title = wide("ミニキャラに表示する画像 / GIF を選択");
    let mut ofn = OPENFILENAMEW {
        lStructSize: std::mem::size_of::<OPENFILENAMEW>() as u32,
        hwndOwner: owner,
        lpstrFilter: PCWSTR(filter.as_ptr()),
        lpstrFile: windows::core::PWSTR(file.as_mut_ptr()),
        nMaxFile: file.len() as u32,
        lpstrTitle: PCWSTR(title.as_ptr()),
        Flags: OFN_FILEMUSTEXIST | OFN_PATHMUSTEXIST | OFN_NOCHANGEDIR,
        ..Default::default()
    };
    unsafe {
        if GetOpenFileNameW(&mut ofn).as_bool() {
            let len = file.iter().position(|c| *c == 0).unwrap_or(file.len());
            Some(String::from_utf16_lossy(&file[..len]))
        } else {
            None
        }
    }
}
