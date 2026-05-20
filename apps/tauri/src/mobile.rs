//! Mobile entry point for Ostler Hub desktop (iOS/Android).

#[tauri::mobile_entry_point]
fn main() {
    zeroclaw_desktop::run();
}
