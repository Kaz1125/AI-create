//! Notification-area icon, right-click menu and "start with Windows" setting.

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::Registry::*;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::config::{Anchor, Config, Item, Mode};
use crate::WM_APP_TRAY;

const ID_ITEM: u16 = 100;
const ID_MODE: u16 = 200;
const ID_ANCHOR_TRAY: u16 = 300;
const ID_ANCHOR_LEFT: u16 = 301;
const ID_FONT: u16 = 310;
const ID_FULLSCREEN: u16 = 320;
const ID_AUTOSTART: u16 = 330;
const ID_OPEN_CONFIG: u16 = 340;
const ID_EXIT: u16 = 399;
const FONT_SIZES: [(f32, &str); 3] = [(11.0, "小"), (13.0, "中"), (15.0, "大")];

pub enum Action {
    ToggleItem(Item),
    SetMode(Mode),
    SetAnchor(Anchor),
    FontSize(f32),
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
