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
        // App icon (exe icon in Explorer, and resource 1 for the tray icon).
        // assets/icon.res is precompiled from assets/icon.rc so no resource compiler is
        // needed: link.exe (MSVC) and lld (zig / GNU) both accept .res input.
        let res = std::path::Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("assets").join("icon.res");
        println!("cargo:rustc-link-arg-bins={}", res.display());
        println!("cargo:rerun-if-changed=assets/icon.res");
    }
    println!("cargo:rerun-if-changed=build.rs");
}
