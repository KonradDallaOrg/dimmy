//! Embedded model catalog — the single source of truth for the cloud
//! models Dimmy offers per provider.
//!
//! The JSON in `assets/model-catalog.json` is compiled into the core via
//! [`include_str!`] and served to every host UI through the
//! `dimmy_model_catalog_json()` FFI. Windows (System.Text.Json) and macOS
//! (Codable) therefore deserialize the *same* embedded bytes — they cannot
//! drift apart the way the old hand-synced per-platform model lists did.
//!
//! Only cloud (API) models live here. Local models (Whisper / Parakeet /
//! Gemma / Phi) have download + bundle + feature-flag semantics and stay in
//! host code, injected alongside this catalog at picker-build time.

/// The raw catalog JSON, embedded at build time from
/// `assets/model-catalog.json` (resolved relative to the crate root so it
/// works regardless of where this source file sits).
pub const MODEL_CATALOG_JSON: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../assets/model-catalog.json"
));

#[cfg(test)]
mod tests {
    use super::MODEL_CATALOG_JSON;
    use serde_json::Value;

    const TASKS: [&str; 3] = ["stt", "llm", "recap"];

    fn parse() -> Value {
        serde_json::from_str(MODEL_CATALOG_JSON)
            .expect("embedded model-catalog.json must be valid JSON")
    }

    #[test]
    fn catalog_parses_and_has_providers() {
        let v = parse();
        assert_eq!(v["schema"], 2, "schema version must be 2");
        let providers = v["providers"]
            .as_array()
            .expect("providers must be an array");
        assert!(
            !providers.is_empty(),
            "catalog must list at least one provider"
        );
    }

    #[test]
    fn every_provider_is_well_formed() {
        let v = parse();
        for p in v["providers"].as_array().unwrap() {
            let id = p["id"].as_str().expect("provider id must be a string");
            assert!(!id.is_empty(), "provider id must be non-empty");
            assert!(
                !p["name"].as_str().unwrap_or("").is_empty(),
                "provider {id} must have a non-empty name"
            );
            assert!(
                !p["icon"].as_str().unwrap_or("").is_empty(),
                "provider {id} must have a non-empty icon (host renders Assets/Providers/<icon>.svg)"
            );
            let models = p["models"]
                .as_array()
                .unwrap_or_else(|| panic!("provider {id} models must be an array"));
            assert!(
                !models.is_empty(),
                "provider {id} must list at least one model"
            );
        }
    }

    #[test]
    fn every_model_is_well_formed_and_task_urls_exist() {
        let v = parse();
        for p in v["providers"].as_array().unwrap() {
            let id = p["id"].as_str().unwrap();
            let llm_url = p["llm_url"].as_str().unwrap_or("");
            let stt_url = p["stt_url"].as_str().unwrap_or("");
            let mut seen = std::collections::HashSet::new();
            for m in p["models"].as_array().unwrap() {
                let mid = m["id"].as_str().expect("model id must be a string");
                assert!(!mid.is_empty(), "provider {id} has a model with empty id");
                assert!(
                    seen.insert(mid.to_string()),
                    "provider {id} has duplicate model id {mid}"
                );
                assert!(
                    !m["label"].as_str().unwrap_or("").is_empty(),
                    "model {id}/{mid} must have a non-empty label"
                );
                let tasks = m["tasks"]
                    .as_array()
                    .unwrap_or_else(|| panic!("model {id}/{mid} tasks must be an array"));
                assert!(
                    !tasks.is_empty(),
                    "model {id}/{mid} must declare at least one task"
                );
                for t in tasks {
                    let t = t.as_str().expect("task must be a string");
                    assert!(TASKS.contains(&t), "model {id}/{mid} has unknown task {t}");
                    // A model usable for a task is useless without the endpoint
                    // it resolves to — the bug class this catalog exists to kill.
                    if t == "llm" || t == "recap" {
                        assert!(
                            !llm_url.is_empty(),
                            "model {id}/{mid} declares task {t} but provider has no llm_url"
                        );
                    }
                    if t == "stt" {
                        assert!(
                            !stt_url.is_empty(),
                            "model {id}/{mid} declares task stt but provider has no stt_url"
                        );
                    }
                }
            }
        }
    }
}
