//! Build script for the Victauri demo application.

fn main() {
    // `examples/tauri3` builds this app's `src/main.rs` against Tauri 3 and sets this cfg there.
    println!("cargo::rustc-check-cfg=cfg(victauri_tauri3)");
    tauri_build::build();
}
