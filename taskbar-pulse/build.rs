//! Embeds the Windows application manifest. Declaring Windows 8+ compatibility is
//! required for layered *child* windows, which the taskbar-embedded mode uses.
use embed_manifest::manifest::{ActiveCodePage, DpiAwareness, SupportedOS::*};
use embed_manifest::{embed_manifest, new_manifest};

fn main() {
    if std::env::var_os("CARGO_CFG_WINDOWS").is_some() {
        embed_manifest(
            new_manifest("TaskbarPulse")
                .supported_os(Windows7..=Windows10)
                .dpi_awareness(DpiAwareness::PerMonitorV2)
                .active_code_page(ActiveCodePage::Utf8),
        )
        .expect("unable to embed manifest");
    }
    println!("cargo:rerun-if-changed=build.rs");
}
