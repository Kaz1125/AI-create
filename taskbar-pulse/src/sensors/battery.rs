//! PC battery, Bluetooth device batteries and Xbox (XInput) controller batteries.

use windows::core::{GUID, PCWSTR};
use windows::Win32::Devices::DeviceAndDriverInstallation::*;
use windows::Win32::Devices::Properties::*;
use windows::Win32::Foundation::DEVPROPKEY;
use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
use windows::Win32::UI::Input::XboxController::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeviceKind {
    Mouse,
    Keyboard,
    Gamepad,
    Headset,
    Other,
}

#[derive(Clone, Debug)]
pub struct DeviceBattery {
    pub name: String,
    pub kind: DeviceKind,
    pub percent: u8,
}

#[derive(Clone, Copy, Debug)]
pub struct PcBattery {
    pub percent: u8,
    pub charging: bool,
}

/// Laptop battery. None on desktops (no system battery).
pub fn pc_battery() -> Option<PcBattery> {
    let mut status = SYSTEM_POWER_STATUS::default();
    unsafe { GetSystemPowerStatus(&mut status) }.ok()?;
    // BatteryFlag 128 = no system battery, 255 = unknown. BatteryLifePercent 255 = unknown.
    if status.BatteryFlag == 128 || status.BatteryFlag == 255 || status.BatteryLifePercent > 100 {
        return None;
    }
    Some(PcBattery {
        percent: status.BatteryLifePercent,
        charging: status.ACLineStatus == 1,
    })
}

/// Battery level Windows shows in Settings > Bluetooth & devices.
/// This key is not in Microsoft's public headers; it is widely used by open-source
/// tools (property type: byte, 0-100) and may be missing for some devices.
const DEVPKEY_BLUETOOTH_BATTERY: DEVPROPKEY = DEVPROPKEY {
    fmtid: GUID::from_u128(0x104EA319_6EE2_4701_BD47_8DDBF425BBE5),
    pid: 2,
};

fn classify(name: &str) -> DeviceKind {
    let n = name.to_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| n.contains(w));
    if has(&["mouse", "mx master", "mx anywhere", "trackball", "マウス"]) {
        DeviceKind::Mouse
    } else if has(&["keyboard", "keys", "キーボード"]) {
        DeviceKind::Keyboard
    } else if has(&[
        "controller", "gamepad", "dualsense", "dualshock", "joy-con", "pro controller", "xbox",
        "8bitdo", "コントローラー",
    ]) {
        DeviceKind::Gamepad
    } else if has(&["headset", "headphone", "buds", "airpods", "earbuds", "wh-", "wf-", "ヘッド"]) {
        DeviceKind::Headset
    } else {
        DeviceKind::Other
    }
}

fn read_property(set: HDEVINFO, data: &SP_DEVINFO_DATA, key: &DEVPROPKEY, buf: &mut [u8]) -> Option<(DEVPROPTYPE, usize)> {
    let mut prop_type = DEVPROPTYPE::default();
    let mut size = 0u32;
    unsafe { SetupDiGetDevicePropertyW(set, data, key, &mut prop_type, Some(buf), Some(&mut size), 0) }
        .ok()?;
    Some((prop_type, size as usize))
}

fn utf16_bytes_to_string(bytes: &[u8]) -> String {
    let words: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .take_while(|w| *w != 0)
        .collect();
    String::from_utf16_lossy(&words)
}

/// Enumerates present PnP devices that report a Bluetooth battery level.
/// This walks every device node, so callers should run it rarely (about once a minute).
pub fn bluetooth_batteries() -> Vec<DeviceBattery> {
    let mut out: Vec<DeviceBattery> = Vec::new();
    let set = match unsafe {
        SetupDiGetClassDevsW(None, PCWSTR::null(), None, DIGCF_ALLCLASSES | DIGCF_PRESENT)
    } {
        Ok(set) => set,
        Err(_) => return out,
    };
    let mut index = 0u32;
    loop {
        let mut data = SP_DEVINFO_DATA {
            cbSize: std::mem::size_of::<SP_DEVINFO_DATA>() as u32,
            ..Default::default()
        };
        if unsafe { SetupDiEnumDeviceInfo(set, index, &mut data) }.is_err() {
            break;
        }
        index += 1;

        let mut buf = [0u8; 8];
        let Some((prop_type, size)) = read_property(set, &data, &DEVPKEY_BLUETOOTH_BATTERY, &mut buf)
        else {
            continue;
        };
        if prop_type != DEVPROP_TYPE_BYTE || size < 1 || buf[0] > 100 {
            continue;
        }
        let percent = buf[0];

        let mut name_buf = [0u8; 512];
        let name = read_property(set, &data, &DEVPKEY_Device_FriendlyName, &mut name_buf)
            .or_else(|| read_property(set, &data, &DEVPKEY_NAME, &mut name_buf))
            .map(|(_, size)| utf16_bytes_to_string(&name_buf[..size.min(name_buf.len())]))
            .unwrap_or_default();
        let name = clean_name(&name);
        if name.is_empty() || out.iter().any(|d| d.name == name) {
            continue;
        }
        out.push(DeviceBattery { kind: classify(&name), name, percent });
    }
    unsafe {
        let _ = SetupDiDestroyDeviceInfoList(set);
    }
    out
}

/// Strips suffixes Windows adds to per-profile device nodes, e.g. "Hands-Free AG".
fn clean_name(name: &str) -> String {
    let mut n = name.trim().to_string();
    for suffix in [" Hands-Free AG", " Hands-Free", " Avrcp Transport", " Stereo"] {
        if let Some(stripped) = n.strip_suffix(suffix) {
            n = stripped.to_string();
        }
    }
    n
}

/// Xbox-compatible controllers via XInput. XInput only reports coarse levels,
/// so they are mapped to approximate percentages (empty 5, low 30, medium 65, full 100).
pub fn xinput_batteries() -> Vec<DeviceBattery> {
    let mut out = Vec::new();
    for user in 0..4u32 {
        let mut info = XINPUT_BATTERY_INFORMATION::default();
        let status = unsafe { XInputGetBatteryInformation(user, BATTERY_DEVTYPE_GAMEPAD, &mut info) };
        if status != 0 {
            continue;
        }
        if info.BatteryType == BATTERY_TYPE_DISCONNECTED || info.BatteryType == BATTERY_TYPE_WIRED {
            continue;
        }
        let percent = match info.BatteryLevel {
            BATTERY_LEVEL_EMPTY => 5,
            BATTERY_LEVEL_LOW => 30,
            BATTERY_LEVEL_MEDIUM => 65,
            _ => 100,
        };
        out.push(DeviceBattery {
            name: format!("Xbox Controller {}", user + 1),
            kind: DeviceKind::Gamepad,
            percent,
        });
    }
    out
}
