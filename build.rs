// Windows: embed an application manifest so wxWidgets gets modern themed controls,
// dark mode and high-DPI support.
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let target = std::env::var("TARGET").unwrap_or_default();
    if target.contains("windows-gnu") {
        // see build/win-shim/vswscanf.c
        println!("cargo:rerun-if-changed=build/win-shim/vswscanf.c");
        if let Err(e) = cc::Build::new().file("build/win-shim/vswscanf.c").try_compile("msu2shim") {
            println!("cargo:warning=vswscanf shim not built ({e}); linking wxWidgets may fail");
        }
    }
    if target.contains("windows") {
        use embed_manifest::manifest::{ActiveCodePage, DpiAwareness, Setting, SupportedOS::*};
        let manifest = embed_manifest::new_manifest("OpenMSU2MINI.App")
            .supported_os(Windows7..=Windows10)
            .active_code_page(ActiveCodePage::Utf8)
            .dpi_awareness(DpiAwareness::PerMonitorV2)
            .long_path_aware(Setting::Enabled);
        if let Err(e) = embed_manifest::embed_manifest(manifest) {
            println!("cargo:warning=could not embed manifest: {e}");
        }
    }
}
