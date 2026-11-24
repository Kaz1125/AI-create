//! Thin wrapper around the Windows Performance Data Helper (PDH) API.
//! PDH needs no admin rights and is what Task Manager-style tools commonly read.

use windows::core::{HSTRING, PCWSTR};
use windows::Win32::System::Performance::*;

const ERROR_SUCCESS: u32 = 0;
/// PDH_FMT_NOCAP100: do not clamp percentages to 100 (needed for "% Processor Utility").
const PDH_FMT_NOCAP100: u32 = 0x0000_8000;

pub struct Query {
    handle: PDH_HQUERY,
}

#[derive(Clone, Copy)]
pub struct Counter(PDH_HCOUNTER);

// PDH handles are plain kernel-side handles; we only use the query from one thread at a time.
unsafe impl Send for Query {}
unsafe impl Send for Counter {}

impl Query {
    pub fn new() -> Option<Query> {
        let mut handle = PDH_HQUERY::default();
        let status = unsafe { PdhOpenQueryW(PCWSTR::null(), 0, &mut handle) };
        (status == ERROR_SUCCESS).then_some(Query { handle })
    }

    /// Adds a counter using its English (locale independent) path. Returns None if the
    /// counter does not exist on this machine (e.g. no GPU counters on old drivers).
    pub fn add(&self, path: &str) -> Option<Counter> {
        let mut counter = PDH_HCOUNTER::default();
        let path = HSTRING::from(path);
        let status = unsafe { PdhAddEnglishCounterW(self.handle, &path, 0, &mut counter) };
        (status == ERROR_SUCCESS).then_some(Counter(counter))
    }

    pub fn collect(&self) -> bool {
        unsafe { PdhCollectQueryData(self.handle) == ERROR_SUCCESS }
    }

    pub fn value(&self, counter: Counter) -> Option<f64> {
        let mut value = PDH_FMT_COUNTERVALUE::default();
        let status = unsafe {
            PdhGetFormattedCounterValue(
                counter.0,
                PDH_FMT(PDH_FMT_DOUBLE.0 | PDH_FMT_NOCAP100),
                None,
                &mut value,
            )
        };
        if status == ERROR_SUCCESS && (value.CStatus == 0 || value.CStatus == 1) {
            Some(unsafe { value.Anonymous.doubleValue })
        } else {
            None
        }
    }

    /// Reads every instance of a wildcard counter, e.g. `\GPU Engine(*)\Utilization Percentage`.
    pub fn array(&self, counter: Counter) -> Vec<(String, f64)> {
        let mut size = 0u32;
        let mut count = 0u32;
        let format = PDH_FMT(PDH_FMT_DOUBLE.0 | PDH_FMT_NOCAP100);
        let status =
            unsafe { PdhGetFormattedCounterArrayW(counter.0, format, &mut size, &mut count, None) };
        if status != PDH_MORE_DATA || size == 0 {
            return Vec::new();
        }
        // Buffer must be 8-byte aligned; allocate as u64s.
        let mut buffer = vec![0u64; (size as usize).div_ceil(8)];
        let items = buffer.as_mut_ptr() as *mut PDH_FMT_COUNTERVALUE_ITEM_W;
        let status = unsafe {
            PdhGetFormattedCounterArrayW(counter.0, format, &mut size, &mut count, Some(items))
        };
        if status != ERROR_SUCCESS {
            return Vec::new();
        }
        let slice = unsafe { std::slice::from_raw_parts(items, count as usize) };
        slice
            .iter()
            .filter(|item| item.FmtValue.CStatus == 0 || item.FmtValue.CStatus == 1)
            .map(|item| {
                let name = unsafe { item.szName.to_string() }.unwrap_or_default();
                (name, unsafe { item.FmtValue.Anonymous.doubleValue })
            })
            .collect()
    }
}

impl Drop for Query {
    fn drop(&mut self) {
        unsafe {
            PdhCloseQuery(self.handle);
        }
    }
}
