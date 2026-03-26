//! Dimmy Linux native UI — GTK4 + libadwaita entry point.

mod hotkey;
mod onboarding;
mod pill_window;
mod settings;
mod state;
mod text_injector;
mod tray;
mod waveform;

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
        // Before creating pill, check onboarding
        if !dimmy_lib::onboarding_completed() {
            onboarding::show_onboarding(app, &state_clone);
            return; // Don't show pill until onboarding done
        }

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

        // Create pill overlay
        let (pill, update_pill_state) =
            pill_window::create_pill_window(app, &state_clone);

        // Connect AppEvent receiver to pill state via glib main loop
        let update_fn = update_pill_state.clone();
        gtk4::glib::spawn_future_local(async move {
            while let Ok(event) = receiver.recv().await {
                log(&format!("AppEvent: {:?}", event));
                match event {
                    state::AppEvent::RecordingStarted => {
                        update_fn(pill_window::PillState::Recording)
                    }
                    state::AppEvent::RecordingStopped => {
                        update_fn(pill_window::PillState::Transcribing)
                    }
                    state::AppEvent::TranscriptionComplete(_) => {
                        update_fn(pill_window::PillState::Completing)
                    }
                    state::AppEvent::LlmComplete(_) => {
                        update_fn(pill_window::PillState::Completing)
                    }
                    state::AppEvent::Error(_) => {
                        update_fn(pill_window::PillState::Error)
                    }
                    state::AppEvent::TranscriptionProgress { .. } => {} // chunk counter
                    state::AppEvent::AmplitudeUpdate(_) => {} // waveform handled by timer
                    state::AppEvent::StyleChanged(_) => {}
                    state::AppEvent::ToneChanged(_) => {}
                }
            }
        });

        pill.present();

        // Start system tray (StatusNotifierItem via D-Bus)
        let _tray_handle = tray::start_tray(&state_clone);
    });

    // Don't pass command-line args to GTK (they're for us, not GTK)
    app.run_with_args::<&str>(&[]);
}
