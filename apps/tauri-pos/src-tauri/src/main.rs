// The diagnostic window is a client, not the runtime. All it does is start the
// Tauri shell defined in the library crate.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    tauri_pos_lib::run();
}
