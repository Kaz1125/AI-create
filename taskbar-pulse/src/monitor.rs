//! Finds the taskbar (or bottom edge) of the monitor the user picked.

use windows::core::{w, BOOL, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::config::Config;

pub struct MonitorInfo {
    pub handle: HMONITOR,
    /// Device name such as \\.\DISPLAY1; stable across restarts on most setups.
    pub name: String,
    pub rect: RECT,
    pub primary: bool,
}

unsafe extern "system" fn collect(mon: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> BOOL {
    let list = &mut *(data.0 as *mut Vec<MonitorInfo>);
    let mut info = MONITORINFOEXW::default();
    info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
    if GetMonitorInfoW(mon, &mut info as *mut _ as *mut MONITORINFO).as_bool() {
        let len = info.szDevice.iter().position(|c| *c == 0).unwrap_or(info.szDevice.len());
        list.push(MonitorInfo {
            handle: mon,
            name: String::from_utf16_lossy(&info.szDevice[..len]),
            rect: info.monitorInfo.rcMonitor,
            primary: info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY != 0,
        });
    }
    TRUE
}

/// All monitors, primary first, then left to right.
pub fn list() -> Vec<MonitorInfo> {
    let mut v: Vec<MonitorInfo> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(None, None, Some(collect), LPARAM(&mut v as *mut _ as isize));
    }
    v.sort_by_key(|m| (!m.primary, m.rect.left, m.rect.top));
    v
}

/// Where the taskbar is and where our strip should go.
pub struct Placement {
    pub taskbar: RECT,
    /// X coordinate our strip should end at when anchored beside the tray / clock.
    pub tray_left: i32,
    pub scale: f32,
}

fn monitor_scale(mon: HMONITOR) -> f32 {
    let (mut x, mut y) = (0u32, 0u32);
    match unsafe { GetDpiForMonitor(mon, MDT_EFFECTIVE_DPI, &mut x, &mut y) } {
        Ok(()) if x > 0 => x as f32 / 96.0,
        _ => 1.0,
    }
}

fn primary_taskbar() -> Option<Placement> {
    unsafe {
        let taskbar = FindWindowW(w!("Shell_TrayWnd"), PCWSTR::null()).ok()?;
        let mut rect = RECT::default();
        GetWindowRect(taskbar, &mut rect).ok()?;
        let dpi = windows::Win32::UI::HiDpi::GetDpiForWindow(taskbar);
        let scale = if dpi == 0 { 1.0 } else { dpi as f32 / 96.0 };
        // Notification area. Present on Windows 10 and 11; fall back to a fixed margin.
        let tray_left = FindWindowExW(Some(taskbar), None, w!("TrayNotifyWnd"), PCWSTR::null())
            .ok()
            .and_then(|tray| {
                let mut r = RECT::default();
                GetWindowRect(tray, &mut r).ok().map(|_| r.left)
            })
            .filter(|l| *l > rect.left && *l < rect.right)
            .unwrap_or(rect.right - (320.0 * scale) as i32);
        Some(Placement { taskbar: rect, tray_left, scale })
    }
}

/// Taskbar of a non-primary monitor (`Shell_SecondaryTrayWnd`, present when
/// "Show my taskbar on all displays" is on). If there is none, use the bottom edge.
fn secondary(mon: &MonitorInfo) -> Placement {
    let scale = monitor_scale(mon.handle);
    unsafe {
        let mut prev: Option<HWND> = None;
        while let Ok(hwnd) = FindWindowExW(None, prev, w!("Shell_SecondaryTrayWnd"), PCWSTR::null()) {
            prev = Some(hwnd);
            if MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) == mon.handle {
                let mut rect = RECT::default();
                if GetWindowRect(hwnd, &mut rect).is_ok() {
                    // Leave room for the clock that secondary taskbars can show on the right.
                    let tray_left = rect.right - (150.0 * scale) as i32;
                    return Placement { taskbar: rect, tray_left, scale };
                }
            }
        }
    }
    let h = (48.0 * scale) as i32;
    let r = mon.rect;
    Placement {
        taskbar: RECT { left: r.left, top: r.bottom - h, right: r.right, bottom: r.bottom },
        tray_left: r.right - (16.0 * scale) as i32,
        scale,
    }
}

pub fn placement(cfg: &Config) -> Option<Placement> {
    if let Some(name) = &cfg.monitor {
        let monitors = list();
        if let Some(m) = monitors.iter().find(|m| &m.name == name) {
            if !m.primary {
                return Some(secondary(m));
            }
        }
    }
    primary_taskbar()
}
