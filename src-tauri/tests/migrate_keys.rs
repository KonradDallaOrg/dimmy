//! Integration test: migrate keys from Windows keyring to local encrypted file.
//! Run with: cargo test --test migrate_keys -- --nocapture

use dimmy_lib::keystore::KeyStore;
use dimmy_lib::provider::{KeyringScope, Provider};

#[test]
fn migrate_keyring_to_local() {
    let store = KeyStore::new();

    let scopes = [
        KeyringScope::Stt(Provider::Groq),
        KeyringScope::Stt(Provider::OpenAI),
        KeyringScope::Stt(Provider::Gemini),
        KeyringScope::Stt(Provider::Deepgram),
        KeyringScope::Llm(Provider::Anthropic),
    ];

    println!("\n=== Step 1: Read keys from OS keyring ===\n");
    let mut found_keys: Vec<(KeyringScope, String)> = Vec::new();
    for scope in &scopes {
        // Load with use_keyring=true to read from OS keyring
        match store.load_key(*scope, true) {
            Some(key) => {
                let preview = if key.len() > 12 {
                    format!("{}...{}", &key[..6], &key[key.len() - 4..])
                } else {
                    "***".to_string()
                };
                println!("  [OK] {} = {}", scope.entry_name(), preview);
                found_keys.push((*scope, key));
            }
            None => {
                println!("  [--] {} = not found", scope.entry_name());
            }
        }
    }

    assert!(
        !found_keys.is_empty(),
        "No keys found in keyring — nothing to migrate"
    );
    println!("\n  Found {} keys in keyring\n", found_keys.len());

    println!("=== Step 2: Save keys to local encrypted file ===\n");
    for (scope, key) in &found_keys {
        store
            .save_key(*scope, key, false)
            .expect(&format!("Failed to save {} to local store", scope));
        println!(
            "  [OK] {} saved to local encrypted store",
            scope.entry_name()
        );
    }

    println!("\n=== Step 3: Verify keys load from local file ===\n");
    // Reload cache from disk to verify persistence
    store.reload_local_cache();
    for (scope, original_key) in &found_keys {
        let loaded = store
            .load_key(*scope, false)
            .expect(&format!("{} not found in local store after save", scope));
        assert_eq!(
            &loaded, original_key,
            "Key mismatch for {} after roundtrip",
            scope
        );
        println!("  [OK] {} roundtrip verified", scope.entry_name());
    }

    println!("\n=== Step 4: Delete keys from OS keyring ===\n");
    for (scope, _) in &found_keys {
        let name = scope.entry_name();
        match keyring::Entry::new("dimmy", &name) {
            Ok(entry) => match entry.delete_credential() {
                Ok(()) => println!("  [OK] {} deleted from keyring", name),
                Err(e) => println!("  [!!] {} delete failed: {}", name, e),
            },
            Err(e) => println!("  [!!] {} entry error: {}", name, e),
        }
    }

    println!("\n=== Step 5: Verify keys still load from local (keyring gone) ===\n");
    for (scope, original_key) in &found_keys {
        // use_keyring=false, keyring deleted — must come from local file
        let loaded = store
            .load_key(*scope, false)
            .expect(&format!("{} not found after keyring deletion", scope));
        assert_eq!(&loaded, original_key, "Key mismatch for {}", scope);
        println!(
            "  [OK] {} loads from local (keyring deleted)",
            scope.entry_name()
        );
    }

    println!(
        "\n=== Migration complete! {} keys migrated ===\n",
        found_keys.len()
    );
}
