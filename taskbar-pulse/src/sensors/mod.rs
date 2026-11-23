//! Hardware sensors. `Sensors::read` is called from a background thread.

pub mod battery;
pub mod pdh;
pub mod temps;

use std::collections::HashMap;
use std::time::{Duration, Instant};

use battery::{DeviceBattery, PcBattery};
use windows::Win32::System::SystemInformation::*;

#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub cpu: Option<f32>,
    pub p_cores: Option<f32>,
    pub e_cores: Option<f32>,
    pub gpu: Option<f32>,
    pub vram_used_gb: Option<f32>,
    pub mem: Option<f32>,
    pub mem_used_gb: Option<f32>,
    pub disk: Option<f32>,
    pub net_down: Option<f64>,
    pub net_up: Option<f64>,
    pub cpu_temp: Option<f32>,
    pub gpu_temp: Option<f32>,
    pub battery: Option<PcBattery>,
    pub devices: Vec<DeviceBattery>,
}

/// Maps (processor group, logical index) to "is efficiency core".
/// Uses GetSystemCpuSetInformation; on hybrid CPUs the highest EfficiencyClass
/// value marks the performance cores. Returns None on non-hybrid CPUs.
fn core_classes() -> Option<HashMap<(u16, u8), bool>> {
    let mut len = 0u32;
    unsafe {
        let _ = GetSystemCpuSetInformation(None, 0, &mut len, None, None);
    }
    if len == 0 {
        return None;
    }
    let mut buf = vec![0u64; (len as usize).div_ceil(8)];
    let ok = unsafe {
        GetSystemCpuSetInformation(
            Some(buf.as_mut_ptr() as *mut SYSTEM_CPU_SET_INFORMATION),
            len,
            &mut len,
            None,
            None,
        )
    };
    if !ok.as_bool() {
        return None;
    }
    let bytes = unsafe { std::slice::from_raw_parts(buf.as_ptr() as *const u8, len as usize) };
    let mut entries = Vec::new();
    let mut offset = 0usize;
    while offset + 8 <= bytes.len() {
        let info = unsafe { &*(bytes.as_ptr().add(offset) as *const SYSTEM_CPU_SET_INFORMATION) };
        if info.Size == 0 {
            break;
        }
        if info.Type == CpuSetInformation {
            let cs = unsafe { &info.Anonymous.CpuSet };
            entries.push((cs.Group, cs.LogicalProcessorIndex, cs.EfficiencyClass));
        }
        offset += info.Size as usize;
    }
    let max = entries.iter().map(|e| e.2).max()?;
    let min = entries.iter().map(|e| e.2).min()?;
    if max == min {
        return None;
    }
    Some(entries.into_iter().map(|(g, i, c)| ((g, i), c < max)).collect())
}

pub struct Sensors {
    query: Option<pdh::Query>,
    cpu_total: Option<pdh::Counter>,
    cpu_each: Option<pdh::Counter>,
    gpu_engine: Option<pdh::Counter>,
    gpu_mem: Option<pdh::Counter>,
    disk_idle: Option<pdh::Counter>,
    net_down: Option<pdh::Counter>,
    net_up: Option<pdh::Counter>,
    thermal: Option<pdh::Counter>,
    classes: Option<HashMap<(u16, u8), bool>>,
    wmi: temps::WmiTemps,
    nvml: Option<temps::Nvml>,
    last_temps: Instant,
    cached_temps: temps::HwTemps,
    last_devices: Option<Instant>,
    cached_bt: Vec<DeviceBattery>,
}

impl Sensors {
    pub fn new() -> Sensors {
        let query = pdh::Query::new();
        let add = |p: &str| query.as_ref().and_then(|q| q.add(p));
        let s = Sensors {
            cpu_total: add("\\Processor Information(_Total)\\% Processor Utility"),
            cpu_each: add("\\Processor Information(*)\\% Processor Utility"),
            gpu_engine: add("\\GPU Engine(*)\\Utilization Percentage"),
            gpu_mem: add("\\GPU Adapter Memory(*)\\Dedicated Usage"),
            disk_idle: add("\\PhysicalDisk(_Total)\\% Idle Time"),
            net_down: add("\\Network Interface(*)\\Bytes Received/sec"),
            net_up: add("\\Network Interface(*)\\Bytes Sent/sec"),
            thermal: add("\\Thermal Zone Information(*)\\Temperature"),
            query,
            classes: core_classes(),
            wmi: temps::WmiTemps::new(),
            nvml: temps::Nvml::load(),
            last_temps: Instant::now() - Duration::from_secs(60),
            cached_temps: temps::HwTemps::default(),
            last_devices: None,
            cached_bt: Vec::new(),
        };
        // Rate counters need two samples; prime the first one.
        if let Some(q) = &s.query {
            q.collect();
        }
        s
    }

    pub fn is_hybrid(&self) -> bool {
        self.classes.is_some()
    }

    pub fn read(&mut self, want_devices: bool) -> Snapshot {
        let mut snap = Snapshot::default();
        if let Some(q) = &self.query {
            q.collect();
            snap.cpu = self.cpu_total.and_then(|c| q.value(c)).map(|v| v.clamp(0.0, 100.0) as f32);

            if let (Some(c), Some(classes)) = (self.cpu_each, &self.classes) {
                let (mut p, mut pn, mut e, mut en) = (0.0, 0, 0.0, 0);
                for (name, v) in q.array(c) {
                    // Instances look like "0,5" (group,index); skip "_Total" and "0,_Total".
                    let mut parts = name.split(',');
                    let (Some(g), Some(i)) = (parts.next(), parts.next()) else { continue };
                    let (Ok(g), Ok(i)) = (g.parse::<u16>(), i.parse::<u8>()) else { continue };
                    let v = v.clamp(0.0, 100.0);
                    match classes.get(&(g, i)) {
                        Some(true) => {
                            e += v;
                            en += 1;
                        }
                        Some(false) => {
                            p += v;
                            pn += 1;
                        }
                        None => {}
                    }
                }
                snap.p_cores = (pn > 0).then(|| (p / pn as f64) as f32);
                snap.e_cores = (en > 0).then(|| (e / en as f64) as f32);
            }

            if let Some(c) = self.gpu_engine {
                // Task Manager style: sum per engine across processes, then take the busiest engine.
                let mut per_engine: HashMap<String, f64> = HashMap::new();
                for (name, v) in q.array(c) {
                    let key = match (name.find("luid_"), name.find("_engtype_")) {
                        (Some(a), Some(b)) if b > a => name[a..b].to_string(),
                        _ => name.clone(),
                    };
                    *per_engine.entry(key).or_default() += v;
                }
                if !per_engine.is_empty() {
                    let max = per_engine.values().cloned().fold(0.0, f64::max);
                    snap.gpu = Some(max.clamp(0.0, 100.0) as f32);
                }
            }
            if let Some(c) = self.gpu_mem {
                let max = q.array(c).into_iter().map(|(_, v)| v).fold(0.0, f64::max);
                if max > 0.0 {
                    snap.vram_used_gb = Some((max / 1_073_741_824.0) as f32);
                }
            }
            snap.disk = self
                .disk_idle
                .and_then(|c| q.value(c))
                .map(|idle| (100.0 - idle).clamp(0.0, 100.0) as f32);
            let sum = |c: Option<pdh::Counter>| c.map(|c| q.array(c).into_iter().map(|(_, v)| v).sum::<f64>());
            snap.net_down = sum(self.net_down);
            snap.net_up = sum(self.net_up);
        }

        let mut mem = MEMORYSTATUSEX { dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32, ..Default::default() };
        if unsafe { GlobalMemoryStatusEx(&mut mem) }.is_ok() {
            snap.mem = Some(mem.dwMemoryLoad as f32);
            snap.mem_used_gb = Some(((mem.ullTotalPhys - mem.ullAvailPhys) as f64 / 1_073_741_824.0) as f32);
        }

        // WMI is comparatively expensive, so temperatures refresh at most every 2 s.
        if self.last_temps.elapsed() >= Duration::from_secs(2) {
            self.last_temps = Instant::now();
            let mut t = self.wmi.read();
            if t.gpu.is_none() {
                t.gpu = self.nvml.as_ref().and_then(|n| n.gpu_temp());
            }
            if t.cpu.is_none() {
                if let (Some(q), Some(c)) = (&self.query, self.thermal) {
                    // Kelvin -> Celsius; take the hottest zone.
                    t.cpu = q
                        .array(c)
                        .into_iter()
                        .map(|(_, k)| k - 273.15)
                        .filter(|c| *c > 0.0 && *c < 150.0)
                        .fold(None, |acc: Option<f64>, c| Some(acc.map_or(c, |a| a.max(c))))
                        .map(|c| c as f32);
                }
            }
            self.cached_temps = t;
        }
        snap.cpu_temp = self.cached_temps.cpu;
        snap.gpu_temp = self.cached_temps.gpu;

        snap.battery = battery::pc_battery();
        if want_devices {
            // Enumerating every PnP device is slow; refresh Bluetooth batteries once a minute.
            if self.last_devices.is_none_or(|t| t.elapsed() >= Duration::from_secs(60)) {
                self.last_devices = Some(Instant::now());
                self.cached_bt = battery::bluetooth_batteries();
            }
            snap.devices = self.cached_bt.clone();
            snap.devices.extend(battery::xinput_batteries());
        }
        snap
    }
}
