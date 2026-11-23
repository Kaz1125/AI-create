//! Notification-area icon, right-click menu and "start with Windows" setting.

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::Registry::*;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::config::{Anchor, ColorSlot, Config, Item, Mode, Sensitivity, Theme, VizStyle};
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
const ID_THEME: u16 = 400;
const ID_STYLE: u16 = 420;
const ID_FLAG: u16 = 440;
const ID_OPACITY: u16 = 460;
const ID_COLOR: u16 = 480;
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
    Toggle(Flag),
    PanelOpacity(f32),
    PickColor(ColorSlot),
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
        i if (ID_FLAG..ID_FLAG + FLAGS.len() as u16).contains(&i) => Action::Toggle(FLAGS[(i - ID_FLAG) as usize]),
        i if (ID_OPACITY..ID_OPACITY + OPACITIES.len() as u16).contains(&i) => {
            Action::PanelOpacity(OPACITIES[(i - ID_OPACITY) as usize].0)
        }
        i if (ID_COLOR..ID_COLOR + ColorSlot::ALL.len() as u16).contains(&i) => {
            Action::PickColor(ColorSlot::ALL[(i - ID_COLOR) as usize])
        }
        ID_FULLSCREEN => Action::ToggleFullscreenHide,
        ID_AUTOSTART => Action::ToggleAutostart,
        ID_OPEN_CONFIG => Action::OpenConfig,
        ID_EXIT => Action::Exit,
        _ => Action::None,
    }
}

/// Draws a small three-bar meter icon at runtime (no resource compiler needed).
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
    let tip: Vec<u16> = "Taskbar Pulse".encode_utf16().collect();
    data.szTip[..tip.len()].copy_from_slice(&tip);
    data
}

pub fn add(hwnd: HWND) {
    let mut data = icon_data(hwnd);
    data.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
    data.uCallbackMessage = WM_APP_TRAY;
    data.hIcon = make_icon();
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
            if *item == Item::Visualizer && cfg.mode.visualizer_interval_ms().is_none() {
                label += " (標準/高負荷モードで有効)";
            }
            append(items, flags, (ID_ITEM + i as u16) as usize, &label);
        }
        append(menu, MF_POPUP, items.0 as usize, "表示項目");

        let modes = CreatePopupMenu().unwrap_or_default();
        for (i, mode) in Mode::ALL.iter().enumerate() {
            append(modes, MF_STRING | check(cfg.mode == *mode), (ID_MODE + i as u16) as usize, mode.label());
        }
        append(modes, MF_SEPARATOR, 0, "");
        let status = if hardware_active { "現在の描画: GPU" } else { "現在の描画: CPU" };
        append(modes, MF_STRING | MF_GRAYED, 0, status);
        append(menu, MF_POPUP, modes.0 as usize, "動作モード");

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
        let animated = cfg.mode.visualizer_interval_ms().is_some();
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

        let pos = CreatePopupMenu().unwrap_or_default();
        append(pos, MF_STRING | check(cfg.anchor == Anchor::BesideTray), ID_ANCHOR_TRAY as usize, "通知領域の左 (位置リセット)");
        append(pos, MF_STRING | check(cfg.anchor == Anchor::Left), ID_ANCHOR_LEFT as usize, "タスクバーの左端 (位置リセット)");
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
