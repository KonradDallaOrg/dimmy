// Prevents additional console window on Windows in release
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // Set transparent background for WebView2 BEFORE it initializes.
    // This prevents the white flash/background on Windows — the env var
    // must be set before the WebView2 controller is created by Tauri.
    #[cfg(target_os = "windows")]
    std::env::set_var("WEBVIEW2_DEFAULT_BACKGROUND_COLOR", "0");

    dimmy_lib::run();
}
