//! Build script for the Tauri 3 build of the Victauri demo app.

fn main() {
    // Tells the shared `examples/demo-app/src/main.rs` it is being built against Tauri 3.
    println!("cargo::rustc-check-cfg=cfg(victauri_tauri3)");
    println!("cargo::rustc-cfg=victauri_tauri3");
    tauri_build::build();
}
