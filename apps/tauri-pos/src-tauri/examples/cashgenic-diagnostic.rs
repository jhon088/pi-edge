//! DIAGNOSTIC, temporary — not part of the product. A console program, so it
//! prints where it was started from; see `src/cashgenic_diagnostic.rs`.
//!
//! An *example*, not a `src/bin` target, on purpose: Tauri's bundler picks up
//! a package's binary targets, and an example is never one, so this cannot
//! end up inside the PX Kiosk installer. Built only by `.github/workflows/cashgenic-diagnostic.yml`:
//! `cargo build --release --example cashgenic-diagnostic`.
//!
//! `cashgenic-diagnostic 192.168.88.240 44333`

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    std::process::exit(tauri_pos_lib::cashgenic_diagnostic(&args));
}
