//! ABI snapshot test for `dimmy_lib` (cdylib).
//!
//! The native UIs (C# WinUI 3, Swift macOS, Rust GTK) call into Rust via
//! ~50 `dimmy_*` C ABI symbols. A silent rename, drop, or accidental
//! mangling causes a runtime crash on the first call from the UI — exactly
//! how the v0.6.10 ABI mismatch reached production.
//!
//! This test compiles the cdylib in release mode, parses the resulting
//! shared library with the `object` crate, extracts the exported `dimmy_*`
//! symbol set, and compares it against a golden file checked into the
//! repo at `core/tests/fixtures/abi_exports.txt`.
//!
//! The golden file is sorted, one symbol per line. To intentionally update
//! it (after adding/removing an FFI export), run:
//!
//! ```bash
//! UPDATE_ABI=1 cargo test --release --test abi_snapshot --features local-stt
//! ```
//!
//! The test will overwrite the fixture, the diff goes through code review.
//! Do NOT update the fixture without a corresponding update to every UI
//! that consumes the changed symbol.
//!
//! Run normally: `cargo test --release --test abi_snapshot --features local-stt`

#![cfg(feature = "local-stt")]

use std::path::PathBuf;
use std::process::Command;

use object::read::Object;

const FIXTURE_REL: &str = "tests/fixtures/abi_exports.txt";
const SYMBOL_PREFIX: &str = "dimmy_";

/// Repo-relative path to the cdylib produced by `cargo build --release --lib`.
fn cdylib_path() -> PathBuf {
    let target_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("release");

    #[cfg(target_os = "windows")]
    let name = "dimmy_lib.dll";
    #[cfg(target_os = "macos")]
    let name = "libdimmy_lib.dylib";
    #[cfg(all(unix, not(target_os = "macos")))]
    let name = "libdimmy_lib.so";

    target_dir.join(name)
}

/// Build the cdylib if it isn't already on disk, using the same feature
/// flags CI uses for the windows-ui-smoke job. The local-stt feature is
/// the lowest common denominator: every supported platform builds it.
fn ensure_cdylib_built() {
    let path = cdylib_path();
    if path.exists() {
        return;
    }

    eprintln!("[abi] cdylib not found at {:?}, building…", path);
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
    let status = Command::new(env!("CARGO"))
        .args([
            "build",
            "--release",
            "--lib",
            "--features",
            "local-stt",
            "--manifest-path",
        ])
        .arg(&manifest)
        .status()
        .expect("spawn cargo build");
    assert!(status.success(), "cargo build --lib --release failed");
    assert!(
        path.exists(),
        "cdylib still missing after build: {:?}",
        path
    );
}

/// Parse the cdylib and return the sorted set of `dimmy_*` exported symbols.
///
/// Cross-platform notes:
/// - PE (Windows): `Object::exports()` returns names from the export table
///   without the leading underscore. Names are already plain `dimmy_*`.
/// - ELF (Linux): `Object::exports()` returns dynamic-symbol-table names
///   (those with global binding and DEFAULT visibility).
/// - Mach-O (macOS): names appear with a leading underscore (`_dimmy_init`).
///   We strip it so the golden file is identical across platforms.
fn extract_dimmy_exports(lib_path: &std::path::Path) -> Vec<String> {
    let bytes = std::fs::read(lib_path).expect("read cdylib bytes");
    let obj = object::File::parse(&*bytes).expect("parse cdylib as object file");

    let mut names: Vec<String> = obj
        .exports()
        .expect("read export table")
        .iter()
        .filter_map(|e| std::str::from_utf8(e.name()).ok())
        .map(|n| n.strip_prefix('_').unwrap_or(n).to_string())
        .filter(|n| n.starts_with(SYMBOL_PREFIX))
        .collect();

    names.sort();
    names.dedup();
    assert!(
        !names.is_empty(),
        "found zero `{}*` exports — parser misconfiguration?",
        SYMBOL_PREFIX
    );
    names
}

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(FIXTURE_REL)
}

fn read_fixture() -> Vec<String> {
    let path = fixture_path();
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read {} failed: {}", path.display(), e));
    text.lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| l.to_string())
        .collect()
}

fn write_fixture(symbols: &[String]) {
    let path = fixture_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create fixtures dir");
    }
    let mut body = String::from(
        "# ABI snapshot for dimmy_lib (cdylib). DO NOT EDIT BY HAND.\n\
         # Regenerate with: UPDATE_ABI=1 cargo test --release --test abi_snapshot --features local-stt\n\
         # See core/tests/abi_snapshot.rs for what this guards against.\n",
    );
    for s in symbols {
        body.push_str(s);
        body.push('\n');
    }
    std::fs::write(&path, body).expect("write fixture");
    eprintln!(
        "[abi] wrote {} symbols to {}",
        symbols.len(),
        path.display()
    );
}

#[test]
fn dimmy_ffi_exports_match_golden_snapshot() {
    ensure_cdylib_built();
    let actual = extract_dimmy_exports(&cdylib_path());

    if std::env::var_os("UPDATE_ABI").is_some() {
        write_fixture(&actual);
        return;
    }

    let expected = read_fixture();

    let actual_set: std::collections::BTreeSet<&String> = actual.iter().collect();
    let expected_set: std::collections::BTreeSet<&String> = expected.iter().collect();

    let added: Vec<&&String> = actual_set.difference(&expected_set).collect();
    let removed: Vec<&&String> = expected_set.difference(&actual_set).collect();

    if !added.is_empty() || !removed.is_empty() {
        let mut msg = String::from(
            "ABI export set drift detected.\n\n\
             If this is intentional (you added or renamed a `dimmy_*` FFI symbol), run:\n  \
             UPDATE_ABI=1 cargo test --release --test abi_snapshot --features local-stt\n\
             then update every UI that calls into the changed surface.\n\n",
        );
        if !added.is_empty() {
            msg.push_str("New symbols (not in golden):\n");
            for s in &added {
                msg.push_str(&format!("  + {}\n", s));
            }
        }
        if !removed.is_empty() {
            msg.push_str("Missing symbols (present in golden, no longer exported):\n");
            for s in &removed {
                msg.push_str(&format!("  - {}\n", s));
            }
        }
        panic!("{}", msg);
    }

    eprintln!(
        "[abi] OK: {} `{}*` symbols match golden",
        actual.len(),
        SYMBOL_PREFIX
    );
}
