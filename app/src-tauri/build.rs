// Tauri build script (DESIGN.md §14.1).
//
// `tauri_build::build()` reads tauri.conf.json, validates capabilities, and
// wires `generate_context!()` to embed the frontend `dist/` bundle — so the
// frontend MUST be built before this crate's final binary compiles.
fn main() {
    tauri_build::build();
}
