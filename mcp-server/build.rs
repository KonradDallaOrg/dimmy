//! Stamp the bridge with Dimmy's version, not one of its own.
//!
//! `mcp-server` is a separate crate, so it had a separate
//! `version = "0.6.52"` that it reported to Claude Desktop as
//! `serverInfo.version`. Nobody bumped it: by 2026-09-24 the app was at
//! 0.7.7 and the manifest said 0.7.7 while the server introduced itself
//! as 0.6.52, which is the worst of both worlds — two numbers, both
//! shown, disagreeing.
//!
//! There is nothing for a separate number to describe. The bridge ships
//! only inside the app, reads only that app's files, and the startup
//! refresh exists precisely to keep the installed copy in step with the
//! app. The only question anyone asks of that number is "which Dimmy
//! does this belong to".
//!
//! So it is read from `core/Cargo.toml` at build time. A human bumping
//! one number cannot forget the other, because there is only one.

use std::path::Path;

fn main() {
    let core_manifest = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(|p| p.join("core").join("Cargo.toml"));

    let version = core_manifest
        .as_ref()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .as_deref()
        .and_then(parse_package_version)
        .unwrap_or_else(|| {
            // Never silently ship a wrong number: a build that cannot see
            // core/Cargo.toml says so rather than inventing a version.
            panic!("mcp-server: could not read the version from core/Cargo.toml")
        });

    println!("cargo:rustc-env=DIMMY_VERSION={version}");
    if let Some(p) = core_manifest {
        println!("cargo:rerun-if-changed={}", p.display());
    }
}

/// The `version = "x.y.z"` of the `[package]` section, and only that one.
/// Scanning the whole file would happily return a dependency's version
/// from further down.
fn parse_package_version(manifest: &str) -> Option<String> {
    let mut in_package = false;
    for line in manifest.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_package = line == "[package]";
            continue;
        }
        if !in_package {
            continue;
        }
        if let Some(rest) = line.strip_prefix("version") {
            let rest = rest.trim_start();
            let rest = rest.strip_prefix('=')?.trim();
            return Some(rest.trim_matches('"').to_string());
        }
    }
    None
}
