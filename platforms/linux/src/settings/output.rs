//! Output settings tab — LLM enhancement, style, tone, translation.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use dimmy_lib::{log, llm, AppState};
use gtk4::prelude::*;
use libadwaita as adw;
use adw::prelude::*;

/// LLM provider presets: (display name, api_url, model)
const LLM_PRESETS: &[(&str, &str, &str)] = &[
    ("Groq — openai/gpt-oss-120b", "https://api.groq.com/openai/v1/chat/completions", "openai/gpt-oss-120b"),
    ("OpenAI — gpt-4o-mini", "https://api.openai.com/v1/chat/completions", "gpt-4o-mini"),
    ("OpenRouter — llama-3.3-70b", "https://openrouter.ai/api/v1/chat/completions", "meta-llama/llama-3.3-70b-instruct"),
    ("OpenRouter — deepseek-r1", "https://openrouter.ai/api/v1/chat/completions", "deepseek/deepseek-r1"),
    ("Gemini — flash", "https://generativelanguage.googleapis.com/v1beta/openai/chat/completions", "gemini-2.0-flash"),
    ("Anthropic — haiku", "https://api.anthropic.com/v1/messages", "claude-3-5-haiku-latest"),
    ("Anthropic — sonnet", "https://api.anthropic.com/v1/messages", "claude-sonnet-4-20250514"),
    ("Custom", "", ""),
];

/// LLM style display names (order matches `LlmStyle::ALL`)
const STYLE_NAMES: &[&str] = &[
    "Off", "Correct", "Summarize", "Elaborate", "Comprehensible",
    "Professional", "Prompt", "Gen Z", "Boomer", "Emoji",
    "Acronyms", "Imbruttito", "Custom",
];

const TONE_NAMES: &[&str] = &["None", "Formal", "Friendly", "Concise", "Academic"];

const TRANSLATE_OPTIONS: &[(&str, &str)] = &[
    ("None", ""),
    ("English", "en"),
    ("Italiano", "it"),
    ("Deutsch", "de"),
    ("Français", "fr"),
    ("Español", "es"),
];

pub fn create_page(app_state: &Arc<AppState>, _show_advanced: &Rc<Cell<bool>>) -> adw::PreferencesPage {
    let page = adw::PreferencesPage::builder()
        .title("Output")
        .icon_name("document-edit-symbolic")
        .build();

    // ── LLM Enhancement group ────────────────────────────────────────
    let llm_group = adw::PreferencesGroup::builder()
        .title("LLM Enhancement")
        .description("Post-process transcription with AI")
        .build();

    // Enable LLM
    let llm_enabled_row = adw::SwitchRow::builder()
        .title("LLM Enhancement")
        .subtitle("Process transcription through an LLM")
        .build();
    llm_enabled_row.set_active(*app_state.llm_enabled.lock().unwrap_or_else(|e| e.into_inner()));

    let state_en = app_state.clone();
    llm_enabled_row.connect_active_notify(move |row| {
        *state_en.llm_enabled.lock().unwrap_or_else(|e| e.into_inner()) = row.is_active();
        log(&format!("LLM enabled: {}", row.is_active()));
    });
    llm_group.add(&llm_enabled_row);

    // LLM Style
    let style_model = gtk4::StringList::new(STYLE_NAMES);
    let style_row = adw::ComboRow::builder()
        .title("Style")
        .subtitle("How the LLM transforms your text")
        .model(&style_model)
        .build();

    let current_style = *app_state.llm_style.lock().unwrap_or_else(|e| e.into_inner());
    let style_idx = llm::LlmStyle::ALL.iter()
        .position(|s| *s == current_style)
        .unwrap_or(0);
    style_row.set_selected(style_idx as u32);

    // Custom prompt (visible when style = Custom)
    let custom_prompt_row = adw::EntryRow::builder()
        .title("Custom Prompt")
        .build();
    let current_custom = app_state.llm_custom_prompt.lock().unwrap_or_else(|e| e.into_inner()).clone();
    custom_prompt_row.set_text(&current_custom);
    custom_prompt_row.set_visible(current_style == llm::LlmStyle::Custom);

    let state_style = app_state.clone();
    let custom_prompt_ref = custom_prompt_row.clone();
    style_row.connect_selected_notify(move |row| {
        let idx = row.selected() as usize;
        if idx < llm::LlmStyle::ALL.len() {
            let style = llm::LlmStyle::ALL[idx];
            *state_style.llm_style.lock().unwrap_or_else(|e| e.into_inner()) = style;
            custom_prompt_ref.set_visible(style == llm::LlmStyle::Custom);
            log(&format!("LLM style: {}", style.as_str()));
        }
    });

    let state_cp = app_state.clone();
    custom_prompt_row.connect_changed(move |entry| {
        let text = entry.text().to_string();
        *state_cp.llm_custom_prompt.lock().unwrap_or_else(|e| e.into_inner()) = text;
    });

    llm_group.add(&style_row);
    llm_group.add(&custom_prompt_row);

    // ── LLM Provider group ───────────────────────────────────────────
    let provider_group = adw::PreferencesGroup::builder()
        .title("LLM Provider")
        .build();

    let llm_items: Vec<&str> = LLM_PRESETS.iter().map(|(name, _, _)| *name).collect();
    let llm_model = gtk4::StringList::new(&llm_items);
    let llm_provider_row = adw::ComboRow::builder()
        .title("Provider")
        .subtitle("LLM service and model")
        .model(&llm_model)
        .build();

    let current_llm_url = app_state.llm_api_url.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let current_llm_model = app_state.llm_api_model.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let llm_idx = LLM_PRESETS.iter()
        .position(|(_, url, model)| *url == current_llm_url && *model == current_llm_model)
        .unwrap_or(LLM_PRESETS.len() - 1);
    llm_provider_row.set_selected(llm_idx as u32);

    let custom_llm_url_row = adw::EntryRow::builder()
        .title("Custom LLM URL")
        .build();
    custom_llm_url_row.set_text(&current_llm_url);

    let custom_llm_model_row = adw::EntryRow::builder()
        .title("Custom LLM Model")
        .build();
    custom_llm_model_row.set_text(&current_llm_model);

    let is_custom_llm = llm_idx == LLM_PRESETS.len() - 1;
    custom_llm_url_row.set_visible(is_custom_llm);
    custom_llm_model_row.set_visible(is_custom_llm);

    let state_llm = app_state.clone();
    let clu_ref = custom_llm_url_row.clone();
    let clm_ref = custom_llm_model_row.clone();
    llm_provider_row.connect_selected_notify(move |row| {
        let idx = row.selected() as usize;
        if idx < LLM_PRESETS.len() {
            let (_, url, model) = LLM_PRESETS[idx];
            let is_custom_sel = url.is_empty();
            clu_ref.set_visible(is_custom_sel);
            clm_ref.set_visible(is_custom_sel);
            if !is_custom_sel {
                *state_llm.llm_api_url.lock().unwrap_or_else(|e| e.into_inner()) = url.to_string();
                *state_llm.llm_api_model.lock().unwrap_or_else(|e| e.into_inner()) = model.to_string();
                log(&format!("LLM provider: {} / {}", url, model));
            }
        }
    });

    let state_clu = app_state.clone();
    custom_llm_url_row.connect_changed(move |entry| {
        *state_clu.llm_api_url.lock().unwrap_or_else(|e| e.into_inner()) = entry.text().to_string();
    });

    let state_clm = app_state.clone();
    custom_llm_model_row.connect_changed(move |entry| {
        *state_clm.llm_api_model.lock().unwrap_or_else(|e| e.into_inner()) = entry.text().to_string();
    });

    provider_group.add(&llm_provider_row);
    provider_group.add(&custom_llm_url_row);
    provider_group.add(&custom_llm_model_row);

    // Use same API key
    let same_key_row = adw::SwitchRow::builder()
        .title("Use Same API Key")
        .subtitle("Use the STT API key for LLM too")
        .build();
    same_key_row.set_active(*app_state.llm_use_same_key.lock().unwrap_or_else(|e| e.into_inner()));

    // LLM API Key (hidden when using same key)
    let llm_key_row = adw::PasswordEntryRow::builder()
        .title("LLM API Key")
        .build();
    let has_llm_key = app_state.llm_api_key.lock().unwrap_or_else(|e| e.into_inner()).is_some();
    if has_llm_key {
        llm_key_row.set_text("••••••••");
    }
    llm_key_row.set_visible(!same_key_row.is_active());

    let llm_key_ref = llm_key_row.clone();
    let state_sk = app_state.clone();
    same_key_row.connect_active_notify(move |row| {
        let active = row.is_active();
        *state_sk.llm_use_same_key.lock().unwrap_or_else(|e| e.into_inner()) = active;
        llm_key_ref.set_visible(!active);
        log(&format!("LLM use same key: {}", active));
    });

    let state_lk = app_state.clone();
    llm_key_row.connect_changed(move |entry| {
        let text = entry.text().to_string();
        if !text.is_empty() && text != "••••••••" {
            *state_lk.llm_api_key.lock().unwrap_or_else(|e| e.into_inner()) = Some(text.clone());
            let use_kr = *state_lk.use_keyring.lock().unwrap_or_else(|e| e.into_inner());
            let url = state_lk.llm_api_url.lock().unwrap_or_else(|e| e.into_inner()).clone();
            let provider = dimmy_lib::provider::Provider::from_url(&url);
            let scope = dimmy_lib::provider::KeyringScope::Llm(provider);
            if let Err(e) = dimmy_lib::save_key_with_store(&state_lk.key_store, scope, &text, use_kr) {
                log(&format!("WARN: failed to save LLM API key: {}", e));
            }
        }
    });

    provider_group.add(&same_key_row);
    provider_group.add(&llm_key_row);

    // Keep in clipboard
    let clipboard_row = adw::SwitchRow::builder()
        .title("Keep in Clipboard")
        .subtitle("Copy transcription to clipboard after pasting")
        .build();
    clipboard_row.set_active(*app_state.keep_in_clipboard.lock().unwrap_or_else(|e| e.into_inner()));

    let state_cb = app_state.clone();
    clipboard_row.connect_active_notify(move |row| {
        *state_cb.keep_in_clipboard.lock().unwrap_or_else(|e| e.into_inner()) = row.is_active();
        log(&format!("Keep in clipboard: {}", row.is_active()));
    });
    provider_group.add(&clipboard_row);

    page.add(&llm_group);
    page.add(&provider_group);

    // ── Advanced group ───────────────────────────────────────────────
    let advanced_group = adw::PreferencesGroup::builder()
        .title("Advanced")
        .description("Tone, translation, custom prompt")
        .build();

    // Tone
    let tone_model = gtk4::StringList::new(TONE_NAMES);
    let tone_row = adw::ComboRow::builder()
        .title("Tone")
        .subtitle("Writing style modifier")
        .model(&tone_model)
        .build();

    let current_tone = *app_state.llm_tone.lock().unwrap_or_else(|e| e.into_inner());
    let tone_idx = llm::LlmTone::ALL.iter()
        .position(|t| *t == current_tone)
        .unwrap_or(0);
    tone_row.set_selected(tone_idx as u32);

    let state_tone = app_state.clone();
    tone_row.connect_selected_notify(move |row| {
        let idx = row.selected() as usize;
        if idx < llm::LlmTone::ALL.len() {
            let tone = llm::LlmTone::ALL[idx];
            *state_tone.llm_tone.lock().unwrap_or_else(|e| e.into_inner()) = tone;
            log(&format!("LLM tone: {}", tone.as_str()));
        }
    });
    advanced_group.add(&tone_row);

    // Translate to
    let translate_items: Vec<&str> = TRANSLATE_OPTIONS.iter().map(|(name, _)| *name).collect();
    let translate_model = gtk4::StringList::new(&translate_items);
    let translate_row = adw::ComboRow::builder()
        .title("Translate To")
        .subtitle("Translate output to another language")
        .model(&translate_model)
        .build();

    let current_translate = app_state.llm_translate_to.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let translate_idx = TRANSLATE_OPTIONS.iter()
        .position(|(_, code)| *code == current_translate)
        .unwrap_or(0);
    translate_row.set_selected(translate_idx as u32);

    let state_tr = app_state.clone();
    translate_row.connect_selected_notify(move |row| {
        let idx = row.selected() as usize;
        if idx < TRANSLATE_OPTIONS.len() {
            let code = TRANSLATE_OPTIONS[idx].1.to_string();
            *state_tr.llm_translate_to.lock().unwrap_or_else(|e| e.into_inner()) = code.clone();
            log(&format!("Translate to: {}", code));
        }
    });
    advanced_group.add(&translate_row);

    // Advanced group hidden by default
    advanced_group.set_visible(false);
    let show_adv = _show_advanced.clone();
    let adv_ref = advanced_group.clone();
    page.connect_map(move |_| {
        adv_ref.set_visible(show_adv.get());
    });

    page.add(&advanced_group);
    page
}
