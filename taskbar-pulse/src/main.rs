//! Taskbar Pulse: shows hardware usage, now-playing media and an audio visualizer
//! on the Windows taskbar.
#![windows_subsystem = "windows"]

mod anim;
mod audio;
mod config;
mod mascot;
mod media;
mod monitor;
mod render;
mod sensors;
mod tray;

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::System::Com::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::HiDpi::*;
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture};
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use config::{Anchor, Config, Item};
use media::{MediaCommand, MediaInfo};
use sensors::battery::DeviceKind;
use sensors::Snapshot;

pub const WM_APP_SENSORS: u32 = WM_APP + 1;
pub const WM_APP_MEDIA: u32 = WM_APP + 2;
pub const WM_APP_TRAY: u32 = WM_APP + 3;
/// Sent to the controller when the strip window was destroyed by someone else
/// (e.g. Explorer restarted and took its child windows with it).
const WM_APP_RECREATE: u32 = WM_APP + 4;
/// A mascot image finished loading on its worker thread.
const WM_APP_MASCOT: u32 = WM_APP + 5;
const TIMER_ANIM: usize = 1;
const TIMER_POSITION: usize = 2;

pub fn battery_kind_glyph(kind: DeviceKind) -> char {
    match kind {
        DeviceKind::Mouse => '\u{E962}',
        DeviceKind::Keyboard => '\u{E765}',
        DeviceKind::Gamepad => '\u{E7FC}',
        DeviceKind::Headset => '\u{E7F6}',
        DeviceKind::Other => '\u{E702}', // Bluetooth
    }
}

/// Hidden top-level window that owns the tray icon, timers and worker messages.
/// It outlives the strip window, which may be re-created.
static CTRL: AtomicIsize = AtomicIsize::new(0);
static EXITING: AtomicBool = AtomicBool::new(false);

fn ctrl_hwnd() -> HWND {
    HWND(CTRL.load(Ordering::Relaxed) as _)
}

struct App {
    /// The visible strip.
    hwnd: HWND,
    /// Taskbar window the strip is currently embedded in (None = floating topmost window).
    parent: Option<HWND>,
    /// Last position given to the embedded child (client coordinates of `parent`).
    child_pos: Option<(i32, i32)>,
    config: Config,
    renderer: render::Renderer,
    snapshot: Arc<Mutex<Snapshot>>,
    media: Arc<Mutex<Option<MediaInfo>>>,
    media_tx: Sender<MediaCommand>,
    visualizer: audio::Visualizer,
    sensor_interval: Arc<AtomicU32>,
    want_devices: Arc<AtomicBool>,
    hybrid: bool,
    visual: anim::Visual,
    last_frame: Instant,
    /// The previous animation frame changed nothing on screen.
    anim_idle: bool,
    /// Loaded mascot image and the path it came from.
    mascot_image: Option<(String, Arc<mascot::Animation>)>,
    /// Result handed over from the loader thread: (path, image, picked by the user now).
    mascot_pending: Arc<Mutex<Option<(String, Option<mascot::Animation>, bool)>>>,
    /// Seconds of mascot motion (advances faster under CPU load when enabled).
    mascot_phase: f32,
    last_mascot_frame: usize,
    hidden_for_fullscreen: bool,
    /// Drag state: (cursor x at press, offset at press, moved?)
    drag: Option<Drag>,
    taskbar_created_msg: u32,
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

fn with_app<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    APP.with(|a| a.try_borrow_mut().ok().and_then(|mut a| a.as_mut().map(f)))
}

fn fullscreen_app_active() -> bool {
    unsafe {
        match SHQueryUserNotificationState() {
            Ok(state) => {
                state == QUNS_BUSY || state == QUNS_RUNNING_D3D_FULL_SCREEN || state == QUNS_PRESENTATION_MODE
            }
            Err(_) => false,
        }
    }
}

impl App {
    fn redraw(&mut self) {
        if self.config.hide_in_fullscreen && fullscreen_app_active() {
            if !self.hidden_for_fullscreen {
                self.hidden_for_fullscreen = true;
                unsafe {
                    let _ = ShowWindow(self.hwnd, SW_HIDE);
                }
            }
            return;
        } else if self.hidden_for_fullscreen {
            self.hidden_for_fullscreen = false;
            unsafe {
                let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
            }
        }

        let Some(p) = monitor::placement(&self.config) else { return };
        let media = self.media.lock().unwrap().clone();
        let viz = self.visualizer_active().then(|| self.visualizer.snapshot());
        let peaks = (self.config.peak_hold && viz.is_some()).then(|| self.visual.peaks.clone());
        let mascot = self.mascot_segment();
        let segs = render::build_segments(&self.config, &self.visual, media.as_ref(), viz, peaks, mascot);
        let height = p.taskbar.bottom - p.taskbar.top;
        if height <= 0 {
            return;
        }
        match self.renderer.draw(&segs, height, p.scale, &self.config) {
            Ok((dc, w, h)) => {
                let off = (self.config.offset_x as f32 * p.scale) as i32;
                let x = match self.config.anchor {
                    Anchor::BesideTray => p.tray_left - off - w,
                    Anchor::Left => p.taskbar.left + off,
                };
                self.attach(p.window);
                match self.parent {
                    Some(parent) => {
                        // Child window: position is in the taskbar's client coordinates.
                        let mut pt = POINT { x, y: p.taskbar.top };
                        unsafe {
                            let _ = windows::Win32::Graphics::Gdi::ScreenToClient(parent, &mut pt);
                        }
                        render::present_child(self.hwnd, dc, w, h);
                        if self.child_pos != Some((pt.x, pt.y)) {
                            self.child_pos = Some((pt.x, pt.y));
                            unsafe {
                                let _ = SetWindowPos(self.hwnd, Some(HWND_TOP), pt.x, pt.y, 0, 0, SWP_NOSIZE | SWP_NOACTIVATE);
                            }
                        }
                    }
                    None => render::present(self.hwnd, dc, x, p.taskbar.top, w, h),
                }
            }
            Err(_) => {}
        }
    }

    fn mascot_segment(&self) -> Option<render::Segment> {
        if !self.config.has(Item::Mascot) {
            return None;
        }
        match (self.config.mascot, &self.mascot_image) {
            (config::Character::Image, Some((_, anim))) => Some(render::Segment::Image {
                anim: anim.clone(),
                frame: anim.frame_at((self.mascot_phase as f64 * 1000.0) as u64),
            }),
            // No image chosen yet (or it failed to load): fall back to the slime.
            (config::Character::Image, None) => {
                Some(render::Segment::Character { kind: config::Character::Slime, phase: self.mascot_phase })
            }
            (kind, _) => Some(render::Segment::Character { kind, phase: self.mascot_phase }),
        }
    }

    /// Starts loading `path` on a worker thread. Decoding a big GIF can take a moment, and
    /// while embedded our UI thread shares input with the taskbar, so it must not block.
    /// `user_picked` = show an error if it fails and switch to it when it succeeds.
    fn request_mascot_load(&mut self, path: String, user_picked: bool) {
        let slot = self.mascot_pending.clone();
        let ctrl = CTRL.load(Ordering::Relaxed);
        let _ = std::thread::Builder::new().name("mascot-load".into()).spawn(move || {
            unsafe {
                let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            }
            let anim = mascot::load(std::path::Path::new(&path));
            *slot.lock().unwrap() = Some((path, anim, user_picked));
            unsafe {
                let _ = PostMessageW(Some(HWND(ctrl as _)), WM_APP_MASCOT, WPARAM(0), LPARAM(0));
            }
        });
    }

    /// Handles a finished load. Returns false when a user-picked image failed.
    fn on_mascot_loaded(&mut self) -> bool {
        let Some((path, anim, user_picked)) = self.mascot_pending.lock().unwrap().take() else { return true };
        match anim {
            Some(anim) => {
                self.mascot_image = Some((path.clone(), Arc::new(anim)));
                if user_picked {
                    self.config.mascot = config::Character::Image;
                    self.config.mascot_image = Some(path);
                    if !self.config.has(Item::Mascot) {
                        self.config.toggle(Item::Mascot);
                    }
                }
                self.apply_config();
                true
            }
            None => !user_picked,
        }
    }

    fn smoothing(&self) -> bool {
        self.config.smooth_numbers && self.config.animated()
    }

    fn on_sensors(&mut self) {
        let snap = self.snapshot.lock().unwrap().clone();
        let smooth = self.smoothing();
        self.visual.on_sensors(snap, smooth);
        self.redraw();
        self.keep_on_top();
    }

    fn animate(&mut self) {
        let now = Instant::now();
        let dt = (now - self.last_frame).as_secs_f32().min(0.1);
        self.last_frame = now;
        let moving = self.smoothing() && self.visual.tick(dt);
        let mut busy = moving || self.renderer.marquee_active || self.renderer.transition_active;
        if self.config.has(Item::Mascot) {
            // Faster when the CPU is busy: 0.6x at idle up to 3x at 100 %.
            let cpu = self.visual.shown.cpu.unwrap_or(0.0);
            let speed = if self.config.mascot_cpu_speed { 0.6 + cpu / 100.0 * 2.4 } else { 1.0 };
            self.mascot_phase += dt * speed;
            match (&self.config.mascot, &self.mascot_image) {
                (config::Character::Image, Some((_, anim))) => {
                    // Redraw a GIF only when its frame changes.
                    let f = anim.frame_at((self.mascot_phase as f64 * 1000.0) as u64);
                    busy |= f != self.last_mascot_frame;
                    self.last_mascot_frame = f;
                }
                _ => busy = true,
            }
        }
        if self.visualizer_active() {
            let bars = self.visualizer.snapshot();
            busy |= bars.iter().any(|v| *v > 0.005);
            if self.config.peak_hold {
                busy |= self.visual.update_peaks(&bars, dt);
            }
        }
        // Skip frames while nothing moves; draw one more frame after motion stops.
        if busy || !self.anim_idle {
            self.redraw();
        }
        self.anim_idle = !busy;
    }

    fn visualizer_active(&self) -> bool {
        self.config.has(Item::Visualizer) && self.config.animated()
    }

    /// Applies mode/item changes to the worker threads and timers.
    fn apply_config(&mut self) {
        self.sensor_interval.store(self.config.sensor_ms(), Ordering::Relaxed);
        self.want_devices.store(self.config.has(Item::Devices), Ordering::Relaxed);
        self.renderer.set_hardware(self.config.hardware());
        let _ = self.media_tx.send(MediaCommand::Interval(self.config.sensor_ms().clamp(1000, 5000)));
        self.visualizer.bar_count.store(self.config.visualizer_bars, Ordering::Relaxed);
        self.visualizer.sensitivity.store(self.config.visualizer_sensitivity.code(), Ordering::Relaxed);
        let viz = self.visualizer_active();
        self.visualizer.enabled.store(viz, Ordering::Relaxed);
        self.visual.set_history_len(self.config.sensor_ms());
        // One animation timer drives the visualizer, peak markers, scrolling titles and
        // number easing. Low-load modes have no animation at all.
        let wants_anim = viz
            || self.config.has(Item::Media)
            || self.config.has(Item::Mascot)
            || self.config.smooth_numbers;
        unsafe {
            let _ = KillTimer(Some(ctrl_hwnd()), TIMER_ANIM);
            if let Some(ms) = self.config.frame_ms().filter(|_| wants_anim) {
                SetTimer(Some(ctrl_hwnd()), TIMER_ANIM, ms, None);
            }
        }
        self.last_frame = Instant::now();
        self.config.save();
        self.redraw();
    }

    /// Embeds the strip into `taskbar` (when enabled), or turns it back into a floating
    /// topmost window. If embedding fails, falls back to floating.
    fn attach(&mut self, taskbar: Option<HWND>) {
        let wanted = if self.config.embed { taskbar } else { None };
        if wanted == self.parent && self.parent.is_none_or(|p| unsafe { IsWindow(Some(p)).as_bool() }) {
            return;
        }
        self.child_pos = None;
        unsafe {
            if let Some(parent) = wanted {
                SetWindowLongPtrW(self.hwnd, GWL_STYLE, (WS_CHILD | WS_CLIPSIBLINGS | WS_VISIBLE).0 as isize);
                SetWindowLongPtrW(self.hwnd, GWL_EXSTYLE, (WS_EX_LAYERED | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW).0 as isize);
                if SetParent(self.hwnd, Some(parent)).is_ok() {
                    self.parent = Some(parent);
                    let _ = SetWindowPos(self.hwnd, Some(HWND_TOP), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_FRAMECHANGED);
                    return;
                }
            }
            // Floating mode (or embedding failed).
            let _ = SetParent(self.hwnd, None);
            SetWindowLongPtrW(self.hwnd, GWL_STYLE, (WS_POPUP | WS_VISIBLE).0 as isize);
            SetWindowLongPtrW(
                self.hwnd,
                GWL_EXSTYLE,
                (WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE).0 as isize,
            );
            self.parent = None;
            let _ = SetWindowPos(self.hwnd, Some(HWND_TOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_FRAMECHANGED);
        }
    }

    fn keep_on_top(&mut self) {
        if let Some(parent) = self.parent {
            unsafe {
                if !IsWindow(Some(parent)).as_bool() {
                    // Taskbar went away (Explorer restart); re-attach on the next draw.
                    self.parent = None;
                    self.redraw();
                    return;
                }
                // Stay above the taskbar's own child windows.
                let _ = SetWindowPos(self.hwnd, Some(HWND_TOP), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
            }
            return;
        }
        unsafe {
            let _ = SetWindowPos(
                self.hwnd,
                Some(HWND_TOPMOST),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOOWNERZORDER,
            );
        }
    }
}

fn lparam_x(lparam: LPARAM) -> i32 {
    (lparam.0 & 0xFFFF) as i16 as i32
}

unsafe extern "system" fn ctrl_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_APP_SENSORS => {
            with_app(|app| app.on_sensors());
            LRESULT(0)
        }
        WM_APP_MEDIA => {
            with_app(|app| app.redraw());
            LRESULT(0)
        }
        WM_TIMER => {
            with_app(|app| match wparam.0 {
                TIMER_ANIM => app.animate(),
                TIMER_POSITION => app.keep_on_top(),
                _ => {}
            });
            LRESULT(0)
        }
        WM_APP_TRAY => {
            let event = (lparam.0 & 0xFFFF) as u32;
            if event == WM_RBUTTONUP || event == WM_LBUTTONUP || event == WM_CONTEXTMENU {
                show_menu(hwnd);
            }
            LRESULT(0)
        }
        WM_COMMAND => {
            handle_command(hwnd, (wparam.0 & 0xFFFF) as u16);
            LRESULT(0)
        }
        WM_DISPLAYCHANGE | WM_DPICHANGED | WM_SETTINGCHANGE => {
            with_app(|app| app.redraw());
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_APP_MASCOT => {
            let ok = with_app(|app| app.on_mascot_loaded()).unwrap_or(true);
            if !ok {
                MessageBoxW(
                    Some(hwnd),
                    w!("画像を読み込めませんでした。GIF / PNG / JPEG / WebP などの画像ファイルを選んでください。"),
                    w!("Taskbar Pulse"),
                    MB_OK | MB_ICONWARNING,
                );
            }
            LRESULT(0)
        }
        WM_APP_RECREATE => {
            with_app(|app| {
                if let Ok(strip) = create_strip() {
                    app.hwnd = strip;
                    app.parent = None;
                    app.child_pos = None;
                    let _ = ShowWindow(strip, SW_SHOWNOACTIVATE);
                    app.redraw();
                }
            });
            LRESULT(0)
        }
        WM_DESTROY => {
            tray::remove(hwnd);
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => {
            let recreated = with_app(|app| app.taskbar_created_msg).unwrap_or(0);
            if recreated != 0 && msg == recreated {
                // Explorer restarted: the tray icon must be added again.
                tray::add(hwnd);
                with_app(|app| {
                    app.parent = None;
                    app.redraw();
                });
                return LRESULT(0);
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
    }
}

/// Left-button drag on the strip.
#[derive(Clone, Copy)]
enum Drag {
    /// Ctrl+drag or drag from the empty edge: slide the whole strip along the taskbar.
    Strip { start_x: i32, start_off: i32, moved: bool },
    /// Drag on an item: reorder it live among the visible items.
    Item { item: Item, start_x: i32, moved: bool },
}

impl App {
    /// Swaps the dragged item with a neighbour once the cursor passes that neighbour's centre.
    fn drag_item_to(&mut self, item: Item, x: f32) {
        let ranges = &self.renderer.item_ranges;
        let Some(i) = ranges.iter().position(|r| r.2 == item) else { return };
        let centre = |j: usize| (ranges[j].0 + ranges[j].1) / 2.0;
        let j = if i > 0 && x < centre(i - 1) {
            i - 1
        } else if i + 1 < ranges.len() && x > centre(i + 1) {
            i + 1
        } else {
            return;
        };
        let mut visible: Vec<Item> = ranges.iter().map(|r| r.2).collect();
        visible.swap(i, j);
        // Items that are on but currently have no data (e.g. no battery) keep their slots.
        let mut next = visible.into_iter();
        let shown: Vec<Item> = self
            .config
            .shown()
            .into_iter()
            .map(|it| if self.renderer.item_ranges.iter().any(|r| r.2 == it) { next.next().unwrap_or(it) } else { it })
            .collect();
        self.config.set_shown_order(&shown);
        self.redraw();
    }
}

/// Window procedure of the visible strip: mouse interaction only.
unsafe extern "system" fn strip_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_RBUTTONUP => {
            show_menu(ctrl_hwnd());
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            let mut pt = POINT::default();
            let _ = GetCursorPos(&mut pt);
            let x = lparam_x(lparam) as f32;
            let ctrl = wparam.0 & 0x0008 != 0; // MK_CONTROL
            with_app(|app| {
                let hit = app.renderer.item_ranges.iter().find(|r| x >= r.0 - 7.0 && x <= r.1 + 7.0).map(|r| r.2);
                app.drag = Some(match hit {
                    Some(item) if !ctrl => Drag::Item { item, start_x: pt.x, moved: false },
                    _ => Drag::Strip { start_x: pt.x, start_off: app.config.offset_x, moved: false },
                });
            });
            SetCapture(hwnd);
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            if wparam.0 & 0x0001 != 0 {
                // MK_LBUTTON: drag the strip horizontally along the taskbar.
                let mut pt = POINT::default();
                let _ = GetCursorPos(&mut pt);
                let x = lparam_x(lparam) as f32;
                with_app(|app| match app.drag {
                    Some(Drag::Strip { start_x, start_off, moved }) => {
                        let dx = pt.x - start_x;
                        if moved || dx.abs() > 3 {
                            let scale = monitor::placement(&app.config).map(|p| p.scale).unwrap_or(1.0);
                            let delta = (dx as f32 / scale) as i32;
                            app.config.offset_x = match app.config.anchor {
                                Anchor::BesideTray => start_off - delta,
                                Anchor::Left => start_off + delta,
                            };
                            app.drag = Some(Drag::Strip { start_x, start_off, moved: true });
                            app.redraw();
                        }
                    }
                    Some(Drag::Item { item, start_x, moved }) => {
                        if moved || (pt.x - start_x).abs() > 4 {
                            if !moved {
                                app.drag = Some(Drag::Item { item, start_x, moved: true });
                                app.renderer.highlight = Some(item);
                                app.redraw();
                            }
                            if let Ok(c) = LoadCursorW(None, IDC_SIZEWE) {
                                SetCursor(Some(c));
                            }
                            app.drag_item_to(item, x);
                        }
                    }
                    None => {}
                });
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let _ = ReleaseCapture();
            let x = lparam_x(lparam) as f32;
            with_app(|app| {
                let moved = matches!(app.drag, Some(Drag::Strip { moved: true, .. } | Drag::Item { moved: true, .. }));
                app.drag = None;
                if app.renderer.highlight.take().is_some() {
                    app.redraw();
                }
                if moved {
                    app.config.save();
                } else if let Some((_, _, button)) =
                    app.renderer.media_buttons.iter().find(|(a, b, _)| x >= *a && x <= *b)
                {
                    let cmd = match button {
                        render::MediaButton::Previous => MediaCommand::Previous,
                        render::MediaButton::PlayPause => MediaCommand::TogglePlayPause,
                        render::MediaButton::Next => MediaCommand::Next,
                    };
                    let _ = app.media_tx.send(cmd);
                }
            });
            LRESULT(0)
        }
        WM_MOUSEWHEEL => {
            // Scroll over the strip: next / previous track.
            let delta = ((wparam.0 >> 16) & 0xFFFF) as i16;
            with_app(|app| {
                let cmd = if delta < 0 { MediaCommand::Next } else { MediaCommand::Previous };
                let _ = app.media_tx.send(cmd);
            });
            LRESULT(0)
        }
        WM_DESTROY => {
            // Destroyed by someone else (e.g. Explorer restart destroys its child windows):
            // ask the controller to build a new strip.
            if !EXITING.load(Ordering::Relaxed) {
                let _ = PostMessageW(Some(ctrl_hwnd()), WM_APP_RECREATE, WPARAM(0), LPARAM(0));
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

fn create_strip() -> windows::core::Result<HWND> {
    unsafe {
        let instance = GetModuleHandleW(None)?;
        CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
            w!("TaskbarPulseOverlay"),
            w!("Taskbar Pulse"),
            WS_POPUP,
            0,
            0,
            10,
            10,
            None,
            None,
            Some(instance.into()),
            None,
        )
    }
}

fn show_menu(hwnd: HWND) {
    let Some((cfg, hybrid, hw)) = with_app(|app| (app.config.clone(), app.hybrid, app.renderer.is_hardware())) else {
        return;
    };
    tray::show_menu(hwnd, &cfg, hybrid, hw);
}

fn handle_command(hwnd: HWND, id: u16) {
    match tray::decode(id) {
        tray::Action::ToggleItem(item) => {
            with_app(|app| {
                app.config.toggle(item);
                app.apply_config();
            });
        }
        tray::Action::SetMode(mode) => {
            with_app(|app| {
                app.config.mode = mode;
                app.apply_config();
            });
        }
        tray::Action::SetAnchor(anchor) => {
            with_app(|app| {
                app.config.anchor = anchor;
                app.config.offset_x = Config::default().offset_x;
                app.apply_config();
            });
        }
        tray::Action::SetSensitivity(sens) => {
            with_app(|app| {
                app.config.visualizer_sensitivity = sens;
                app.apply_config();
            });
        }
        tray::Action::FontSize(size) => {
            with_app(|app| {
                app.config.font_size = size;
                app.apply_config();
            });
        }
        tray::Action::SetTheme(theme) => {
            with_app(|app| {
                app.config.theme = theme;
                app.apply_config();
            });
        }
        tray::Action::SetTrackAnim(a) => {
            with_app(|app| {
                app.config.track_anim = a;
                app.apply_config();
            });
        }
        tray::Action::SetVizStyle(style) => {
            with_app(|app| {
                app.config.visualizer_style = style;
                app.apply_config();
            });
        }
        tray::Action::Toggle(flag) => {
            with_app(|app| {
                let c = &mut app.config;
                let v = match flag {
                    tray::Flag::PeakHold => &mut c.peak_hold,
                    tray::Flag::Sparklines => &mut c.sparklines,
                    tray::Flag::ColorByValue => &mut c.color_by_value,
                    tray::Flag::SmoothNumbers => &mut c.smooth_numbers,
                    tray::Flag::Panel => &mut c.panel,
                    tray::Flag::AlbumArt => &mut c.album_art,
                    tray::Flag::Marquee => &mut c.marquee,
                };
                *v = !*v;
                app.apply_config();
            });
        }
        tray::Action::PanelOpacity(o) => {
            with_app(|app| {
                app.config.panel_opacity = o;
                app.config.panel = true;
                app.apply_config();
            });
        }
        tray::Action::PickColor(slot) => {
            let Some(current) = with_app(|app| app.config.color_slot(slot)) else { return };
            if let Some(rgb) = tray::pick_color(hwnd, current) {
                with_app(|app| {
                    app.config.set_color(slot, rgb);
                    app.apply_config();
                });
            }
        }
        tray::Action::SetCharacter(kind) => {
            with_app(|app| {
                app.config.mascot = kind;
                if !app.config.has(Item::Mascot) {
                    app.config.toggle(Item::Mascot);
                }
                app.apply_config();
            });
        }
        tray::Action::PickImage => {
            if let Some(path) = tray::pick_image(hwnd) {
                with_app(|app| app.request_mascot_load(path, true));
            }
        }
        tray::Action::ToggleMascotSpeed => {
            with_app(|app| {
                app.config.mascot_cpu_speed = !app.config.mascot_cpu_speed;
                app.apply_config();
            });
        }
        tray::Action::SetMonitor(name) => {
            with_app(|app| {
                app.config.monitor = name;
                app.config.offset_x = Config::default().offset_x;
                app.apply_config();
            });
        }
        tray::Action::SetSensorOverride(v) => {
            with_app(|app| {
                app.config.sensor_interval_override_ms = v;
                app.apply_config();
            });
        }
        tray::Action::SetFpsOverride(v) => {
            with_app(|app| {
                app.config.fps_override = v;
                app.apply_config();
            });
        }
        tray::Action::SetRender(r) => {
            with_app(|app| {
                app.config.render = r;
                app.apply_config();
            });
        }
        tray::Action::ToggleFullscreenHide => {
            with_app(|app| {
                app.config.hide_in_fullscreen = !app.config.hide_in_fullscreen;
                app.apply_config();
            });
        }
        tray::Action::ToggleAutostart => tray::set_autostart(!tray::autostart_enabled()),
        tray::Action::OpenConfig => {
            with_app(|app| app.config.save());
            let path: Vec<u16> = Config::path().to_string_lossy().encode_utf16().chain([0]).collect();
            unsafe {
                ShellExecuteW(None, w!("open"), PCWSTR(path.as_ptr()), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL);
            }
        }
        tray::Action::MoveItem(index, to) => {
            with_app(|app| {
                app.config.move_item(index, to);
                app.apply_config();
            });
        }
        tray::Action::ResetOrder => {
            with_app(|app| {
                app.config.reset_order();
                app.apply_config();
            });
        }
        tray::Action::ToggleEmbed => {
            with_app(|app| {
                app.config.embed = !app.config.embed;
                app.apply_config();
            });
        }
        tray::Action::Exit => unsafe {
            EXITING.store(true, Ordering::Relaxed);
            let strip = with_app(|app| {
                let _ = app.media_tx.send(MediaCommand::Quit);
                app.hwnd
            });
            if let Some(strip) = strip {
                let _ = DestroyWindow(strip);
            }
            let _ = DestroyWindow(hwnd);
        },
        tray::Action::None => {}
    }
}

fn main() -> windows::core::Result<()> {
    unsafe {
        // Single instance.
        let _mutex = CreateMutexW(None, true, w!("Local\\TaskbarPulse.SingleInstance"))?;
        if GetLastError() == ERROR_ALREADY_EXISTS {
            return Ok(());
        }
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?;
        // Process-wide COM security, needed for WMI (temperature) queries. Must run before
        // any other thread starts using COM.
        let _ = CoInitializeSecurity(
            None,
            -1,
            None,
            None,
            RPC_C_AUTHN_LEVEL_DEFAULT,
            RPC_C_IMP_LEVEL_IMPERSONATE,
            None,
            EOAC_NONE,
            None,
        );

        let config = Config::load();
        let instance = GetModuleHandleW(None)?;
        let cursor = LoadCursorW(None, IDC_ARROW)?;
        RegisterClassW(&WNDCLASSW {
            lpfnWndProc: Some(strip_proc),
            hInstance: instance.into(),
            lpszClassName: w!("TaskbarPulseOverlay"),
            hCursor: cursor,
            ..Default::default()
        });
        RegisterClassW(&WNDCLASSW {
            lpfnWndProc: Some(ctrl_proc),
            hInstance: instance.into(),
            lpszClassName: w!("TaskbarPulseController"),
            ..Default::default()
        });
        // Hidden controller window (never shown). Not message-only, so it still receives
        // the "TaskbarCreated" broadcast after Explorer restarts.
        let ctrl = CreateWindowExW(
            WS_EX_TOOLWINDOW,
            w!("TaskbarPulseController"),
            w!("Taskbar Pulse Controller"),
            WS_POPUP,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(instance.into()),
            None,
        )?;
        CTRL.store(ctrl.0 as isize, Ordering::Relaxed);
        let hwnd = create_strip()?;

        // --- Worker threads ---
        let snapshot = Arc::new(Mutex::new(Snapshot::default()));
        let sensor_interval = Arc::new(AtomicU32::new(config.sensor_ms()));
        let want_devices = Arc::new(AtomicBool::new(config.has(Item::Devices)));
        let hwnd_raw = ctrl.0 as isize;
        let (hybrid_tx, hybrid_rx) = channel();
        {
            let (snapshot, interval, want_devices) = (snapshot.clone(), sensor_interval.clone(), want_devices.clone());
            std::thread::Builder::new()
                .name("sensors".into())
                .spawn(move || {
                    let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
                    let mut sensors = sensors::Sensors::new();
                    let _ = hybrid_tx.send(sensors.is_hybrid());
                    // First rate sample needs a short gap.
                    std::thread::sleep(Duration::from_millis(300));
                    loop {
                        let snap = sensors.read(want_devices.load(Ordering::Relaxed));
                        *snapshot.lock().unwrap() = snap;
                        let _ = PostMessageW(Some(HWND(hwnd_raw as _)), WM_APP_SENSORS, WPARAM(0), LPARAM(0));
                        std::thread::sleep(Duration::from_millis(interval.load(Ordering::Relaxed) as u64));
                    }
                })
                .expect("spawn sensor thread");
        }

        let media_state = Arc::new(Mutex::new(None));
        let (media_tx, media_rx) = channel();
        {
            let shared = media_state.clone();
            let interval = config.sensor_ms().clamp(1000, 5000);
            std::thread::Builder::new()
                .name("media".into())
                .spawn(move || {
                    let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
                    media::run(shared, media_rx, interval, move || {
                        let _ = PostMessageW(Some(HWND(hwnd_raw as _)), WM_APP_MEDIA, WPARAM(0), LPARAM(0));
                    });
                })
                .expect("spawn media thread");
        }

        let viz_on = config.has(Item::Visualizer) && config.animated();
        let visualizer =
            audio::Visualizer::start(config.visualizer_bars, viz_on, config.visualizer_sensitivity.code());
        let hybrid = hybrid_rx.recv_timeout(Duration::from_secs(5)).unwrap_or(false);

        let app = App {
            hwnd,
            parent: None,
            child_pos: None,
            renderer: render::Renderer::new()?,
            config,
            snapshot,
            media: media_state,
            media_tx,
            visualizer,
            sensor_interval,
            want_devices,
            hybrid,
            visual: anim::Visual::new(),
            last_frame: Instant::now(),
            anim_idle: false,
            mascot_image: None,
            mascot_pending: Arc::new(Mutex::new(None)),
            mascot_phase: 0.0,
            last_mascot_frame: usize::MAX,
            hidden_for_fullscreen: false,
            drag: None,
            taskbar_created_msg: RegisterWindowMessageW(w!("TaskbarCreated")),
        };
        APP.with(|a| *a.borrow_mut() = Some(app));
        with_app(|app| {
            if let (config::Character::Image, Some(p)) = (app.config.mascot, app.config.mascot_image.clone()) {
                app.request_mascot_load(p, false);
            }
            app.apply_config();
        });

        tray::add(ctrl);
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        SetTimer(Some(ctrl), TIMER_POSITION, 2000, None);

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        APP.with(|a| a.borrow_mut().take());
        CoUninitialize();
        Ok(())
    }
}
