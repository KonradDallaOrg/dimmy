//! Import keys from .env into local encrypted store and verify roundtrip.
//! Run with: cargo test --test import_and_verify -- --nocapture
//!
//! Requires: .env file in project root with GROQ_KEY, OPENAI_KEY, GEMINI_KEY,
//! DEEPGRAM_KEY, ANTHROPIC_KEY

use dimmy_lib::keystore::KeyStore;
use dimmy_lib::provider::{KeyringScope, Provider};

fn load_env() -> std::collections::HashMap<String, String> {
    let env_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join(".env");
    let mut map = std::collections::HashMap::new();
    if let Ok(data) = std::fs::read_to_string(&env_path) {
        for line in data.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some((k, v)) = line.split_once('=') {
                map.insert(k.trim().to_string(), v.trim().to_string());
            }
        }
    }
    map
}

#[test]
fn import_keys_to_local_store() {
    let env = load_env();
    let store = KeyStore::new();

    let scopes: Vec<(KeyringScope, &str)> = vec![
        (KeyringScope::Stt(Provider::Groq), "GROQ_KEY"),
        (KeyringScope::Stt(Provider::OpenAI), "OPENAI_KEY"),
        (KeyringScope::Stt(Provider::Gemini), "GEMINI_KEY"),
        (KeyringScope::Stt(Provider::Deepgram), "DEEPGRAM_KEY"),
        (KeyringScope::Llm(Provider::Anthropic), "ANTHROPIC_KEY"),
    ];

    let mut found: Vec<(KeyringScope, String)> = Vec::new();
    for (scope, env_var) in &scopes {
        if let Some(key) = env.get(*env_var) {
            if !key.is_empty() {
                found.push((*scope, key.clone()));
            }
        }
    }

    assert!(
        !found.is_empty(),
        "No keys found in .env — create .env with GROQ_KEY, OPENAI_KEY, etc."
    );

    println!(
        "\n=== Step 1: Save {} keys to local encrypted file ===\n",
        found.len()
    );
    for (scope, key) in &found {
        store
            .save_key(*scope, key, false)
            .unwrap_or_else(|e| panic!("Failed to save {}: {}", scope, e));
        println!("  [OK] {} saved ({} chars)", scope.entry_name(), key.len());
    }

    println!("\n=== Step 2: Reload cache from disk ===\n");
    store.reload_local_cache();
    println!("  [OK] Cache reloaded");

    println!("\n=== Step 3: Verify all keys decrypt correctly ===\n");
    for (scope, original) in &found {
        let loaded = store
            .load_key(*scope, false)
            .unwrap_or_else(|| panic!("{} not found after save+reload", scope));
        assert_eq!(&loaded, original, "Key mismatch for {}", scope);
        let preview = if loaded.len() > 12 {
            format!("{}...{}", &loaded[..6], &loaded[loaded.len() - 4..])
        } else {
            "***".to_string()
        };
        println!("  [OK] {} = {} (verified)", scope.entry_name(), preview);
    }

    println!(
        "\n=== All {} keys imported and verified! ===\n",
        found.len()
    );

    // Show where the file is
    if let Some(dir) = dirs::config_dir() {
        let path = dir.join("dimmy").join("keys.enc");
        if path.exists() {
            let meta = std::fs::metadata(&path).unwrap();
            println!("  File: {}", path.display());
            println!("  Size: {} bytes", meta.len());
        }
    }
    println!();
}
