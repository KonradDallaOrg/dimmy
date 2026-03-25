//! General settings tab — language, API key, theme, STT provider, audio device.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use dimmy_lib::{log, AppState};
use gtk4::prelude::*;
use libadwaita as adw;
use adw::prelude::*;

/// STT provider presets: (display name, api_url, model)
const STT_PRESETS: &[(&str, &str, &str)] = &[
    ("Groq — whisper-large-v3-turbo", "https://api.groq.com/openai/v1/audio/transcriptions", "whisper-large-v3-turbo"),
    ("Groq — whisper-large-v3", "https://api.groq.com/openai/v1/audio/transcriptions", "whisper-large-v3"),
    ("Groq — distil-whisper-large-v3-en", "https://api.groq.com/openai/v1/audio/transcriptions", "distil-whisper-large-v3-en"),
    ("OpenAI — whisper-1", "https://api.openai.com/v1/audio/transcriptions", "whisper-1"),
    ("OpenAI — gpt-4o-transcribe", "https://api.openai.com/v1/audio/transcriptions", "gpt-4o-transcribe"),
    ("OpenAI — gpt-4o-mini-transcribe", "https://api.openai.com/v1/audio/transcriptions", "gpt-4o-mini-transcribe"),
    ("Deepgram — nova-3", "https://api.deepgram.com/v1/listen", "nova-3"),
    ("Deepgram — nova-2", "https://api.deepgram.com/v1/listen", "nova-2"),
    ("Gemini — flash", "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.0-flash:generateContent", "gemini-2.0-flash"),
    ("Gemini — pro", "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.0-pro:generateContent", "gemini-2.0-pro"),
    ("Custom", "", ""),
];

const LANGUAGES: &[(&str, &str)] = &[
    ("Auto", ""),
    ("Italiano", "it"),
    ("English", "en"),
    ("Español", "es"),
    ("Français", "fr"),
    ("Deutsch", "de"),
    ("Português", "pt"),
];

pub fn create_page(app_state: &Arc<AppState>, _show_advanced: &Rc<Cell<bool>>) -> adw::PreferencesPage {
    let page = adw::PreferencesPage::builder()
        .title("General")
        .icon_name("preferences-system-symbolic")
        .build();

    // ── Basic group ──────────────────────────────────────────────────
    let basic_group = adw::PreferencesGroup::builder()
        .title("General")
        .build();

    // Language
    let lang_items: Vec<&str> = LANGUAGES.iter().map(|(name, _)| *name).collect();
    let lang_model = gtk4::StringList::new(&lang_items);
    let lang_row = adw::ComboRow::builder()
        .title("Language")
        .subtitle("Speech recognition language")
        .model(&lang_model)
        .build();

    // Set current value
    let current_lang = app_state.language.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let lang_idx = LANGUAGES.iter().position(|(_, code)| *code == current_lang).unwrap_or(0);
    lang_row.set_selected(lang_idx as u32);

    let state_lang = app_state.clone();
    lang_row.connect_selected_notify(move |row| {
        let idx = row.selected() as usize;
        if idx < LANGUAGES.len() {
            let code = LANGUAGES[idx].1.to_string();
            *state_lang.language.lock().unwrap_or_else(|e| e.into_inner()) = code.clone();
            log(&format!("Language set to: {}", code));
        }
    });
    basic_group.add(&lang_row);

    // API Key
    let api_key_row = adw::PasswordEntryRow::builder()
        .title("API Key")
        .build();

    let has_key = app_state.api_key.lock().unwrap_or_else(|e| e.into_inner()).is_some();
    if has_key {
        api_key_row.set_text("••••••••");
    }

    let state_key = app_state.clone();
    api_key_row.connect_changed(move |entry| {
        let text = entry.text().to_string();
        if !text.is_empty() && text != "••••••••" {
            *state_key.api_key.lock().unwrap_or_else(|e| e.into_inner()) = Some(text.clone());
            // Also persist to keyring if enabled
            let use_kr = *state_key.use_keyring.lock().unwrap_or_else(|e| e.into_inner());
            let url = state_key.api_url.lock().unwrap_or_else(|e| e.into_inner()).clone();
            let provider = dimmy_lib::provider::Provider::from_url(&url);
            let scope = dimmy_lib::provider::KeyringScope::Stt(provider);
            if let Err(e) = dimmy_lib::save_key_with_store(&state_key.key_store, scope, &text, use_kr) {
                log(&format!("WARN: failed to save API key: {}", e));
            }
        }
    });
    basic_group.add(&api_key_row);

    // Theme
    let theme_items = gtk4::StringList::new(&["Auto", "Light", "Dark"]);
    let theme_row = adw::ComboRow::builder()
        .title("Theme")
        .subtitle("Application color scheme")
        .model(&theme_items)
        .build();

    theme_row.connect_selected_notify(move |row| {
        let manager = adw::StyleManager::default();
        let scheme = match row.selected() {
            0 => adw::ColorScheme::Default,
            1 => adw::ColorScheme::ForceLight,
            2 => adw::ColorScheme::ForceDark,
            _ => adw::ColorScheme::Default,
        };
        manager.set_color_scheme(scheme);
        log(&format!("Theme set to index: {}", row.selected()));
    });
    basic_group.add(&theme_row);

    // Launch at login
    let autostart_row = adw::SwitchRow::builder()
        .title("Launch at login")
        .subtitle("Start Dimmy when you log in")
        .build();
    autostart_row.connect_active_notify(move |row| {
        log(&format!("TODO: Launch at login = {}", row.is_active()));
    });
    basic_group.add(&autostart_row);

    page.add(&basic_group);

    // ── Advanced group ───────────────────────────────────────────────
    let advanced_group = adw::PreferencesGroup::builder()
        .title("Advanced")
        .description("STT provider, audio device, preprocessing")
        .build();

    // STT Provider
    let stt_items: Vec<&str> = STT_PRESETS.iter().map(|(name, _, _)| *name).collect();
    let stt_model = gtk4::StringList::new(&stt_items);
    let stt_row = adw::ComboRow::builder()
        .title("STT Provider")
        .subtitle("Speech-to-text service and model")
        .model(&stt_model)
        .build();

    // Match current URL+model to a preset
    let current_url = app_state.api_url.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let current_model = app_state.api_model.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let stt_idx = STT_PRESETS.iter()
        .position(|(_, url, model)| *url == current_url && *model == current_model)
        .unwrap_or(STT_PRESETS.len() - 1); // default to Custom
    stt_row.set_selected(stt_idx as u32);

    // Custom URL and model rows (visible only when Custom selected)
    let custom_url_row = adw::EntryRow::builder()
        .title("Custom STT URL")
        .build();
    custom_url_row.set_text(&current_url);

    let custom_model_row = adw::EntryRow::builder()
        .title("Custom STT Model")
        .build();
    custom_model_row.set_text(&current_model);

    let is_custom = stt_idx == STT_PRESETS.len() - 1;
    custom_url_row.set_visible(is_custom);
    custom_model_row.set_visible(is_custom);

    let state_stt = app_state.clone();
    let custom_url_ref = custom_url_row.clone();
    let custom_model_ref = custom_model_row.clone();
    stt_row.connect_selected_notify(move |row| {
        let idx = row.selected() as usize;
        if idx < STT_PRESETS.len() {
            let (_, url, model) = STT_PRESETS[idx];
            let is_custom_sel = url.is_empty();
            custom_url_ref.set_visible(is_custom_sel);
            custom_model_ref.set_visible(is_custom_sel);
            if !is_custom_sel {
                *state_stt.api_url.lock().unwrap_or_else(|e| e.into_inner()) = url.to_string();
                *state_stt.api_model.lock().unwrap_or_else(|e| e.into_inner()) = model.to_string();
                log(&format!("STT provider set to: {} / {}", url, model));
            }
        }
    });

    let state_curl = app_state.clone();
    custom_url_row.connect_changed(move |entry| {
        let text = entry.text().to_string();
        *state_curl.api_url.lock().unwrap_or_else(|e| e.into_inner()) = text;
    });

    let state_cmodel = app_state.clone();
    custom_model_row.connect_changed(move |entry| {
        let text = entry.text().to_string();
        *state_cmodel.api_model.lock().unwrap_or_else(|e| e.into_inner()) = text;
    });

    advanced_group.add(&stt_row);
    advanced_group.add(&custom_url_row);
    advanced_group.add(&custom_model_row);

    // Prompt
    let prompt_row = adw::EntryRow::builder()
        .title("Prompt")
        .build();
    let current_prompt = app_state.prompt.lock().unwrap_or_else(|e| e.into_inner()).clone();
    prompt_row.set_text(&current_prompt);

    let state_prompt = app_state.clone();
    prompt_row.connect_changed(move |entry| {
        let text = entry.text().to_string();
        *state_prompt.prompt.lock().unwrap_or_else(|e| e.into_inner()) = text;
    });
    advanced_group.add(&prompt_row);

    // Audio device
    let devices = dimmy_lib::audio::list_input_devices();
    let mut device_names: Vec<&str> = vec!["Default"];
    let dev_strings: Vec<String> = devices;
    let dev_refs: Vec<&str> = dev_strings.iter().map(|s| s.as_str()).collect();
    device_names.extend(dev_refs.iter());
    let device_model = gtk4::StringList::new(&device_names);
    let device_row = adw::ComboRow::builder()
        .title("Audio Device")
        .subtitle("Microphone input")
        .model(&device_model)
        .build();

    let current_device = app_state.selected_device.lock().unwrap_or_else(|e| e.into_inner()).clone();
    if let Some(ref dev) = current_device {
        if let Some(pos) = dev_strings.iter().position(|d| d == dev) {
            device_row.set_selected((pos + 1) as u32); // +1 for "Default"
        }
    }

    let state_dev = app_state.clone();
    device_row.connect_selected_notify(move |row| {
        let idx = row.selected() as usize;
        if idx == 0 {
            *state_dev.selected_device.lock().unwrap_or_else(|e| e.into_inner()) = None;
            log("Audio device set to: Default");
        } else if idx - 1 < dev_strings.len() {
            let name = dev_strings[idx - 1].clone();
            *state_dev.selected_device.lock().unwrap_or_else(|e| e.into_inner()) = Some(name.clone());
            log(&format!("Audio device set to: {}", name));
        }
    });
    advanced_group.add(&device_row);

    // Mic volume (input gain)
    let gain_row = adw::ActionRow::builder()
        .title("Mic Volume")
        .subtitle("Input gain (10%–100%)")
        .build();
    let gain_scale = gtk4::Scale::with_range(gtk4::Orientation::Horizontal, 10.0, 100.0, 5.0);
    gain_scale.set_hexpand(true);
    gain_scale.set_valign(gtk4::Align::Center);
    let current_gain = f32::from_bits(
        app_state.input_gain.load(std::sync::atomic::Ordering::Relaxed),
    );
    gain_scale.set_value((current_gain * 100.0) as f64);

    let state_gain = app_state.clone();
    gain_scale.connect_value_changed(move |scale| {
        let pct = scale.value() as f32;
        assert!(pct >= 10.0 && pct <= 100.0, "Gain out of range: {}", pct);
        let gain = pct / 100.0;
        assert!(gain.is_finite(), "Gain must be finite, got: {}", gain);
        state_gain.input_gain.store(
            gain.to_bits(),
            std::sync::atomic::Ordering::Relaxed,
        );
    });
    gain_row.add_suffix(&gain_scale);
    advanced_group.add(&gain_row);

    // Preprocessing
    let preproc_row = adw::SwitchRow::builder()
        .title("Preprocessing")
        .subtitle("Highpass filter, VAD, AGC")
        .build();
    preproc_row.set_active(*app_state.preprocessing_enabled.lock().unwrap_or_else(|e| e.into_inner()));

    let state_pp = app_state.clone();
    preproc_row.connect_active_notify(move |row| {
        *state_pp.preprocessing_enabled.lock().unwrap_or_else(|e| e.into_inner()) = row.is_active();
        log(&format!("Preprocessing: {}", row.is_active()));
    });
    advanced_group.add(&preproc_row);

    // Chunk streaming
    let chunk_row = adw::SwitchRow::builder()
        .title("Chunk Streaming")
        .subtitle("Stream audio in chunks for long recordings")
        .build();
    chunk_row.set_active(*app_state.chunk_streaming_enabled.lock().unwrap_or_else(|e| e.into_inner()));

    let state_chunk = app_state.clone();
    chunk_row.connect_active_notify(move |row| {
        *state_chunk.chunk_streaming_enabled.lock().unwrap_or_else(|e| e.into_inner()) = row.is_active();
        log(&format!("Chunk streaming: {}", row.is_active()));
    });
    advanced_group.add(&chunk_row);

    // Use keyring
    let keyring_row = adw::SwitchRow::builder()
        .title("Use Keyring")
        .subtitle("Store API keys in system keyring")
        .build();
    keyring_row.set_active(*app_state.use_keyring.lock().unwrap_or_else(|e| e.into_inner()));

    let state_kr = app_state.clone();
    keyring_row.connect_active_notify(move |row| {
        *state_kr.use_keyring.lock().unwrap_or_else(|e| e.into_inner()) = row.is_active();
        log(&format!("Use keyring: {}", row.is_active()));
    });
    advanced_group.add(&keyring_row);

    // Advanced group hidden by default — toggled by show_advanced in mod.rs
    advanced_group.set_visible(false);

    // Listen for visibility changes on the page to sync advanced group
    // We use a simple approach: the page checks show_advanced on map
    let show_adv = _show_advanced.clone();
    let adv_group_ref = advanced_group.clone();
    page.connect_map(move |_| {
        adv_group_ref.set_visible(show_adv.get());
    });

    page.add(&advanced_group);

    page
}
