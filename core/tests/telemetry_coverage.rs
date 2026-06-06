//! Telemetry coverage gate (automation Layer 2 — deterministic, no LLM).
//!
//! Rides the normal `cargo test` (so it runs on every PR / CI) and enforces
//! the MECHANICAL half of telemetry hygiene, leaving the judgment half ("does
//! this feature deserve an event?") to the release-time `/telemetry-audit`
//! skill (Layer 3):
//!
//!   1. every `Event` variant is actually emitted somewhere in the core
//!      (or is explicitly listed in `RESERVED` with a reason) — kills the
//!      "defined but never sent" dead variants that rot silently.
//!   2. every `Event` variant appears in the coverage map
//!      (`docs/dev/telemetry-implementation.md`) — keeps code and the
//!      source-of-truth doc in lockstep.
//!
//! A PR that adds an `Event` variant therefore MUST wire an emit (or reserve
//! it) AND list it in the doc, or CI goes red.

use std::fs;
use std::path::{Path, PathBuf};

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read_rel(rel: &str) -> String {
    fs::read_to_string(manifest_dir().join(rel)).unwrap_or_default()
}

/// Variants that are intentionally defined but not yet emitted. Wire them or
/// delete them; do NOT let this list grow silently. Audited 2026-06-07: these
/// are unwired (errors still reach PostHog via `*.failed` + Sentry).
const RESERVED: &[&str] = &[
    "AppUpdateCheck",
    "AppUpdateApplied",
    "ConfigShortcutChanged",
    "PerfTranscribeOverheadPct",
    "ErrorCloudStt",
    "ErrorCloudLlm",
    "ErrorLocalStt",
    "ErrorLocalLlm",
    "ErrorAudioHealth",
];

/// Pull the `Event` enum variant identifiers out of events.rs by brace-matching
/// the `pub enum Event { ... }` block and taking the leading CamelCase token of
/// each non-comment line.
fn event_variants() -> Vec<String> {
    let src = read_rel("src/telemetry/events.rs");
    let start = src
        .find("pub enum Event")
        .expect("`pub enum Event` not found in events.rs");
    let body = &src[start..];
    let open = body.find('{').expect("enum body open brace");
    let mut depth = 0i32;
    let mut end = open;
    for (i, c) in body[open..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    end = open + i;
                    break;
                }
            }
            _ => {}
        }
    }
    let enum_body = &body[open + 1..end];
    let mut vars = Vec::new();
    for line in enum_body.lines() {
        let t = line.trim_start();
        if t.starts_with("//") || t.starts_with('#') || t.is_empty() {
            continue;
        }
        let name: String = t.chars().take_while(|c| c.is_alphanumeric()).collect();
        match name.chars().next() {
            Some(f) if f.is_ascii_uppercase() => vars.push(name),
            _ => {}
        }
    }
    vars.sort();
    vars.dedup();
    assert!(
        vars.len() > 40,
        "parsed too few Event variants ({}) — parser likely broke",
        vars.len()
    );
    vars
}

/// Concatenate every `.rs` under `src/` EXCEPT events.rs (whose `name()` match
/// references every variant and would mask dead ones).
fn core_src_without_events() -> String {
    let mut out = String::new();
    fn walk(dir: &Path, out: &mut String) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().and_then(|s| s.to_str()) == Some("rs")
                && p.file_name().and_then(|s| s.to_str()) != Some("events.rs")
            {
                if let Ok(s) = fs::read_to_string(&p) {
                    out.push_str(&s);
                    out.push('\n');
                }
            }
        }
    }
    walk(&manifest_dir().join("src"), &mut out);
    out
}

#[test]
fn every_event_variant_is_emitted_or_reserved() {
    let vars = event_variants();
    let src = core_src_without_events();
    let orphans: Vec<&String> = vars
        .iter()
        .filter(|v| !RESERVED.contains(&v.as_str()))
        .filter(|v| !src.contains(&format!("Event::{}", v)))
        .collect();
    assert!(
        orphans.is_empty(),
        "Event variants are DEFINED but never emitted. Wire an emit \
         (`crate::telemetry::track(Event::X{{..}})` or a host-bridge arm), \
         or add to RESERVED with a reason: {:?}",
        orphans
    );
}

#[test]
fn every_event_variant_is_documented() {
    let vars = event_variants();
    let doc = read_rel("../docs/dev/telemetry-implementation.md");
    assert!(
        !doc.is_empty(),
        "telemetry-implementation.md not found from CARGO_MANIFEST_DIR/.."
    );
    let missing: Vec<&String> = vars.iter().filter(|v| !doc.contains(v.as_str())).collect();
    assert!(
        missing.is_empty(),
        "Event variants missing from the coverage map in \
         docs/dev/telemetry-implementation.md (add a row): {:?}",
        missing
    );
}
