//! Dimmy Linux native UI — GTK4 + libadwaita entry point.

mod hotkey;
mod state;
mod text_injector;

use dimmy_lib::log;
use libadwaita as adw;
use adw::prelude::*;

fn main() {
    env_logger::init();

    // Panic hook — log to file
    std::panic::set_hook(Box::new(|info| {
        let bt = std::backtrace::Backtrace::force_capture();
        let msg = format!("PANIC: {}\nBacktrace:\n{}", info, bt);
        eprintln!("{}", msg);
        log(&msg);
    }));

    log("=== Dimmy Linux starting ===");

    // Initialize AppState from config + keyring
    let app_state = dimmy_lib::AppState::new_standalone();
    log("AppState initialized");

    let display = text_injector::detect_display_server();
    let paste_method = text_injector::detect_paste_method(display);
    log(&format!(
        "Display server: {:?}, Paste method: {:?}",
        display, paste_method
    ));
    let _hotkey_backend = hotkey::detect_hotkey_backend();

    // Create GTK application
    let app = adw::Application::builder()
        .application_id("com.dimmy.app")
        .build();

    let app_state = std::sync::Arc::new(app_state);
    let state_clone = app_state.clone();

    app.connect_activate(move |app| {
        let (_sender, receiver) = state::create_event_channel();

        // Spawn tokio runtime in background thread
        let _rt_handle = {
            let rt = tokio::runtime::Runtime::new().expect("Failed to create tokio runtime");
            std::thread::spawn(move || {
                rt.block_on(async {
                    tokio::signal::ctrl_c().await.ok();
                });
            })
        };

        // Placeholder window to prove the stack works
        let window = adw::ApplicationWindow::builder()
            .application(app)
            .title("Dimmy")
            .default_width(400)
            .default_height(300)
            .build();

        let label = gtk4::Label::new(Some(&format!(
            "Dimmy Linux — GTK4 + libadwaita\nAppState loaded: api_url={}",
            state_clone.api_url.lock().unwrap_or_else(|e| e.into_inner())
        )));
        window.set_content(Some(&label));

        // Attach event receiver to GTK main loop
        receiver.attach(None, move |event| {
            log(&format!("AppEvent: {:?}", event));
            gtk4::glib::ControlFlow::Continue
        });

        window.present();
    });

    // Don't pass command-line args to GTK (they're for us, not GTK)
    app.run_with_args::<&str>(&[]);
}
