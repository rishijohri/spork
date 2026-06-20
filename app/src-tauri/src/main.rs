// Spork F3-UI desktop binary entrypoint (DESIGN.md §14.1).
//
// The real work lives in the library (`spork_app_lib::run`) so the same builder
// backs the desktop binary, tests, and the mobile entrypoint convention. The
// `windows_subsystem` attribute hides the console window on Windows release
// builds (no effect on macOS/Linux).
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
#![forbid(unsafe_code)]

fn main() {
    spork_app_lib::run();
}
