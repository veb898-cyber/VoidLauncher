fn main() {
    // Test harnesses link the dialog code from tauri-runtime-wry, which
    // imports comctl32's TaskDialogIndirect. The app binary carries the Tauri
    // manifest that activates Common-Controls 6 (tauri-build emits its link
    // arg for bins only), so a bare test executable resolves the import
    // against the legacy comctl32 5.82 and dies at startup with
    // STATUS_ENTRYPOINT_NOT_FOUND. The dependency below is merged into the
    // app manifest as well, where it is the same entry Tauri already asks
    // for.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!(
            "cargo:rustc-link-arg=/MANIFESTDEPENDENCY:type='win32' \
             name='Microsoft.Windows.Common-Controls' version='6.0.0.0' \
             processorArchitecture='*' publicKeyToken='6595b64144ccf1df' language='*'"
        );
    }
    tauri_build::build()
}
