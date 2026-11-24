//! Display-side state: smoothed numbers, value history for sparklines and
//! visualizer peak markers. Everything here runs on the UI thread.

use std::collections::{HashMap, VecDeque};

use crate::config::Item;
use crate::sensors::Snapshot;

/// Samples kept for sparklines (the time span depends on the mode's refresh rate,
/// so the count is derived from it; see `Visual::set_history_len`).
const MAX_HISTORY: usize = 240;

pub struct Visual {
    /// What is drawn. Approaches `target` over time when smoothing is on.
    pub shown: Snapshot,
    target: Snapshot,
    pub history: HashMap<Item, VecDeque<f32>>,
    history_len: usize,
    pub peaks: Vec<f32>,
    peak_hold: Vec<f32>,
}

fn approach(shown: &mut Option<f32>, target: Option<f32>, k: f32) -> bool {
    match (shown.as_mut(), target) {
        (Some(s), Some(t)) => {
            let diff = t - *s;
            if diff.abs() < 0.05 {
                *s = t;
                false
            } else {
                *s += diff * k;
                true
            }
        }
        _ => {
            *shown = target;
            false
        }
    }
}

fn approach64(shown: &mut Option<f64>, target: Option<f64>, k: f32) -> bool {
    let mut s = shown.map(|v| v as f32);
    let moving = approach(&mut s, target.map(|v| v as f32), k);
    *shown = if moving { s.map(|v| v as f64) } else { target };
    moving
}

impl Visual {
    pub fn new() -> Visual {
        Visual {
            shown: Snapshot::default(),
            target: Snapshot::default(),
            history: HashMap::new(),
            history_len: 60,
            peaks: Vec::new(),
            peak_hold: Vec::new(),
        }
    }

    /// Keep about 60 seconds of history at the given sensor interval.
    pub fn set_history_len(&mut self, sensor_interval_ms: u32) {
        self.history_len = ((60_000 / sensor_interval_ms.max(1)) as usize).clamp(12, MAX_HISTORY);
    }

    pub fn on_sensors(&mut self, snap: Snapshot, smooth: bool) {
        let samples = [
            (Item::Cpu, snap.cpu),
            (Item::PCores, snap.p_cores),
            (Item::ECores, snap.e_cores),
            (Item::Gpu, snap.gpu),
            (Item::Vram, snap.vram_used_gb),
            (Item::Memory, snap.mem),
            (Item::Disk, snap.disk),
            (Item::CpuTemp, snap.cpu_temp),
            (Item::GpuTemp, snap.gpu_temp),
            (Item::Network, snap.net_down.map(|v| v as f32)),
        ];
        for (item, value) in samples {
            if let Some(v) = value {
                let h = self.history.entry(item).or_default();
                h.push_back(v);
                while h.len() > self.history_len {
                    h.pop_front();
                }
            }
        }
        // Non-numeric data is never animated.
        self.shown.battery = snap.battery;
        self.shown.devices = snap.devices.clone();
        if !smooth || self.shown.cpu.is_none() {
            self.shown = snap.clone();
        }
        self.target = snap;
    }

    /// Moves the shown numbers toward the latest reading. Returns true while still moving.
    pub fn tick(&mut self, dt: f32) -> bool {
        // Exponential ease-out: covers ~90% of the gap in about 0.3 s.
        let k = 1.0 - (-dt * 7.5).exp();
        let (s, t) = (&mut self.shown, &self.target);
        let mut moving = false;
        moving |= approach(&mut s.cpu, t.cpu, k);
        moving |= approach(&mut s.p_cores, t.p_cores, k);
        moving |= approach(&mut s.e_cores, t.e_cores, k);
        moving |= approach(&mut s.gpu, t.gpu, k);
        moving |= approach(&mut s.vram_used_gb, t.vram_used_gb, k);
        moving |= approach(&mut s.mem, t.mem, k);
        moving |= approach(&mut s.mem_used_gb, t.mem_used_gb, k);
        moving |= approach(&mut s.disk, t.disk, k);
        moving |= approach(&mut s.cpu_temp, t.cpu_temp, k);
        moving |= approach(&mut s.gpu_temp, t.gpu_temp, k);
        moving |= approach64(&mut s.net_down, t.net_down, k);
        moving |= approach64(&mut s.net_up, t.net_up, k);
        moving
    }

    /// Updates the peak markers. Peaks hold for 0.35 s, then fall at 1.2 bar-heights/s.
    /// Returns true while any marker is above zero.
    pub fn update_peaks(&mut self, bars: &[f32], dt: f32) -> bool {
        if self.peaks.len() != bars.len() {
            self.peaks = vec![0.0; bars.len()];
            self.peak_hold = vec![0.0; bars.len()];
        }
        let mut any = false;
        for i in 0..bars.len() {
            if bars[i] >= self.peaks[i] {
                self.peaks[i] = bars[i];
                self.peak_hold[i] = 0.35;
            } else if self.peak_hold[i] > 0.0 {
                self.peak_hold[i] -= dt;
            } else {
                self.peaks[i] = (self.peaks[i] - 1.2 * dt).max(bars[i]).max(0.0);
            }
            any |= self.peaks[i] > 0.01;
        }
        any
    }
}
