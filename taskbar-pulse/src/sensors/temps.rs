//! Temperature sources that work without installing a kernel driver.
//!
//! 1. LibreHardwareMonitor / OpenHardwareMonitor WMI (if the user runs one of them).
//!    They publish sensors to `ROOT\LibreHardwareMonitor` / `ROOT\OpenHardwareMonitor`.
//! 2. NVIDIA NVML (`nvml.dll`, ships with the NVIDIA driver) for NVIDIA GPU temperature.
//! 3. ACPI thermal zones via PDH (handled in `sensors::mod`), as a last resort for CPU.
//!    Note: on many desktops the ACPI value is a motherboard zone, not the CPU die.

use windows::core::{s, w, BSTR, PCWSTR};
use windows::Win32::Foundation::HMODULE;
use windows::Win32::System::Com::*;
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows::Win32::System::Variant::*;
use windows::Win32::System::Wmi::*;

#[derive(Default, Clone, Copy, Debug)]
pub struct HwTemps {
    pub cpu: Option<f32>,
    pub gpu: Option<f32>,
}

pub struct WmiTemps {
    services: Option<IWbemServices>,
}

impl WmiTemps {
    /// COM must already be initialised on the calling thread.
    pub fn new() -> WmiTemps {
        WmiTemps { services: None }
    }

    fn connect() -> Option<IWbemServices> {
        unsafe {
            let locator: IWbemLocator =
                CoCreateInstance(&WbemLocator, None, CLSCTX_INPROC_SERVER).ok()?;
            for ns in ["ROOT\\LibreHardwareMonitor", "ROOT\\OpenHardwareMonitor"] {
                if let Ok(services) = locator.ConnectServer(
                    &BSTR::from(ns),
                    &BSTR::new(),
                    &BSTR::new(),
                    &BSTR::new(),
                    0,
                    &BSTR::new(),
                    None,
                ) {
                    let _ = CoSetProxyBlanket(
                        &services,
                        10, // RPC_C_AUTHN_WINNT
                        0,  // RPC_C_AUTHZ_NONE
                        PCWSTR::null(),
                        RPC_C_AUTHN_LEVEL_CALL,
                        RPC_C_IMP_LEVEL_IMPERSONATE,
                        None,
                        EOAC_NONE,
                    );
                    return Some(services);
                }
            }
            None
        }
    }

    pub fn read(&mut self) -> HwTemps {
        if self.services.is_none() {
            self.services = Self::connect();
        }
        let Some(services) = self.services.as_ref() else {
            return HwTemps::default();
        };
        match query(services) {
            Some(t) => t,
            None => {
                // The monitor app was probably closed; reconnect next time.
                self.services = None;
                HwTemps::default()
            }
        }
    }
}

fn variant_string(v: &VARIANT) -> String {
    unsafe {
        match VariantToStringAlloc(v) {
            Ok(p) => {
                let s = p.to_string().unwrap_or_default();
                CoTaskMemFree(Some(p.0 as _));
                s
            }
            Err(_) => String::new(),
        }
    }
}

fn get(obj: &IWbemClassObject, name: PCWSTR) -> VARIANT {
    let mut v = VARIANT::default();
    unsafe {
        let _ = obj.Get(name, 0, &mut v, None, None);
    }
    v
}

fn query(services: &IWbemServices) -> Option<HwTemps> {
    let enumerator = unsafe {
        services
            .ExecQuery(
                &BSTR::from("WQL"),
                &BSTR::from("SELECT Identifier, Name, Value FROM Sensor WHERE SensorType='Temperature'"),
                WBEM_FLAG_FORWARD_ONLY | WBEM_FLAG_RETURN_IMMEDIATELY,
                None,
            )
            .ok()?
    };
    let mut temps = HwTemps::default();
    let mut cpu_rank = 0u8;
    loop {
        let mut row = [None; 1];
        let mut returned = 0u32;
        let hr = unsafe { enumerator.Next(WBEM_INFINITE, &mut row, &mut returned) };
        if hr.is_err() || returned == 0 {
            break;
        }
        let Some(obj) = row[0].take() else { break };
        let id = variant_string(&get(&obj, w!("Identifier"))).to_lowercase();
        let name = variant_string(&get(&obj, w!("Name"))).to_lowercase();
        let Ok(value) = (unsafe { VariantToDouble(&get(&obj, w!("Value"))) }) else {
            continue;
        };
        let value = value as f32;
        if id.contains("cpu") {
            // Prefer package / Tctl-Tdie sensors over individual cores.
            let rank = if name.contains("package") || name.contains("tctl") || name.contains("tdie") {
                3
            } else if name.contains("core (") || name.contains("average") {
                2
            } else {
                1
            };
            if rank > cpu_rank {
                cpu_rank = rank;
                temps.cpu = Some(value);
            }
        } else if id.contains("gpu") && (name == "gpu core" || name.contains("core")) && temps.gpu.is_none() {
            temps.gpu = Some(value);
        }
    }
    Some(temps)
}

type NvmlInit = unsafe extern "C" fn() -> i32;
type NvmlGetHandle = unsafe extern "C" fn(u32, *mut usize) -> i32;
type NvmlGetTemp = unsafe extern "C" fn(usize, u32, *mut u32) -> i32;

/// NVIDIA GPU temperature through NVML, loaded at runtime so the app works without NVIDIA.
pub struct Nvml {
    device: usize,
    get_temp: NvmlGetTemp,
    _lib: HMODULE,
}

unsafe impl Send for Nvml {}

impl Nvml {
    pub fn load() -> Option<Nvml> {
        unsafe {
            let lib = LoadLibraryW(w!("nvml.dll"))
                .or_else(|_| LoadLibraryW(w!("C:\\Program Files\\NVIDIA Corporation\\NVSMI\\nvml.dll")))
                .ok()?;
            let init: NvmlInit = std::mem::transmute(GetProcAddress(lib, s!("nvmlInit_v2"))?);
            let handle: NvmlGetHandle =
                std::mem::transmute(GetProcAddress(lib, s!("nvmlDeviceGetHandleByIndex_v2"))?);
            let get_temp: NvmlGetTemp =
                std::mem::transmute(GetProcAddress(lib, s!("nvmlDeviceGetTemperature"))?);
            if init() != 0 {
                return None;
            }
            let mut device = 0usize;
            if handle(0, &mut device) != 0 {
                return None;
            }
            Some(Nvml { device, get_temp, _lib: lib })
        }
    }

    pub fn gpu_temp(&self) -> Option<f32> {
        let mut t = 0u32;
        // NVML_TEMPERATURE_GPU = 0
        let rc = unsafe { (self.get_temp)(self.device, 0, &mut t) };
        (rc == 0).then_some(t as f32)
    }
}
