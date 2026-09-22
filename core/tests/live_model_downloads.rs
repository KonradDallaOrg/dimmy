//! Tier A — live contract test for the model downloads (manual, `#[ignore]`,
//! NOT CI). Run before a release:
//!
//! ```bash
//! cargo test --test live_model_downloads -- --ignored --nocapture
//! ```
//!
//! Why this exists: on 2026-09-22 the Parakeet bundle download had been
//! failing for 124 users since June, and no unit test could have caught it —
//! the bug was in the CONTRACT with Hugging Face, not in our logic in
//! isolation. HF moved the repo to Xet storage, and after the redirect the
//! CDN's `ETag` stopped being the file's SHA-256 and became the Xet block id:
//! still 64 hex characters, so `is_sha256` accepted it, and every download
//! died comparing it against the real hash.
//!
//! The only thing that catches that class of failure is asking the real
//! server. It is cheap: the file hashed below is 139 KB.
//!
//! What the contract actually is, verified 2026-09-22:
//!   - the hash lives on the **302 from huggingface.co** as `x-linked-etag`,
//!     NOT on the CDN response the redirect leads to;
//!   - for LFS/Xet files it is the content SHA-256 (64 hex);
//!   - for small files kept in plain git (here `vocab.txt`) it is the git
//!     blob SHA-1 (40 hex), which `is_sha256` correctly rejects, so those
//!     files are fetched without a hash check. That asymmetry is by design.

use sha2::{Digest, Sha256};

const PARAKEET_BASE: &str =
    "https://huggingface.co/istupakov/parakeet-tdt-0.6b-v3-onnx/resolve/main";

/// The LFS-backed half of the bundle: these must carry a content SHA-256.
const LFS_FILES: &[&str] = &[
    "nemo128.onnx",
    "encoder-model.onnx",
    "encoder-model.onnx.data",
    "decoder_joint-model.onnx",
];
/// Smallest LFS file in the bundle: hashing it costs 139 KB of traffic.
const SMALLEST_LFS: &str = "nemo128.onnx";

fn no_redirect_client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .expect("http client")
}

fn linked_etag(client: &reqwest::blocking::Client, url: &str) -> Option<String> {
    let resp = client.head(url).send().ok()?;
    let raw = resp.headers().get("x-linked-etag")?.to_str().ok()?;
    Some(raw.trim_matches('"').to_string())
}

/// Every LFS file we download must still advertise a SHA-256 on the 302.
/// That header IS the integrity check: without it `download_resumable` has no
/// hash to verify against and a truncated body would ship silently.
#[test]
#[ignore]
fn parakeet_lfs_files_still_advertise_a_sha256() {
    let client = no_redirect_client();
    for name in LFS_FILES {
        let url = format!("{PARAKEET_BASE}/{name}");
        let etag = linked_etag(&client, &url).unwrap_or_else(|| {
            panic!(
                "{name}: no x-linked-etag on the redirect — the downloader has no hash to verify"
            )
        });
        assert!(
            dimmy_lib::download::is_sha256(&etag),
            "{name}: x-linked-etag is no longer a sha256 ({etag}) — integrity checking is off for this file"
        );
    }
}

/// The bytes the server sends must hash to what `x-linked-etag` promised.
/// This is the assertion the Parakeet outage would have failed on: the hash
/// the old code picked up (the post-redirect `ETag`) did not describe the
/// bytes, so the download deleted itself on the first file, every time.
#[test]
#[ignore]
fn parakeet_bytes_match_the_linked_etag() {
    let client = no_redirect_client();
    let url = format!("{PARAKEET_BASE}/{SMALLEST_LFS}");
    let expected = linked_etag(&client, &url).expect("x-linked-etag on the redirect");

    let body = reqwest::blocking::get(&url)
        .expect("GET the file")
        .error_for_status()
        .expect("2xx")
        .bytes()
        .expect("body");
    let got = dimmy_lib::download::hex_lower(&Sha256::digest(&body));

    assert_eq!(
        got, expected,
        "{SMALLEST_LFS}: the bytes do not hash to x-linked-etag"
    );
}

/// End to end through OUR downloader, on the real server, into a temp dir:
/// resume logic, hash probe and verification, the whole path the bundle takes.
/// 139 KB. This is the test that goes red if the Parakeet download breaks
/// again, whatever the reason.
#[test]
#[ignore]
fn our_downloader_can_fetch_a_parakeet_file() {
    let dir = std::env::temp_dir().join(format!("dimmy-dl-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let dest = dir.join(SMALLEST_LFS);

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(300))
        .build()
        .expect("client");
    let rt = tokio::runtime::Runtime::new().expect("runtime");
    let result = rt.block_on(dimmy_lib::download::download_resumable(
        &client,
        &format!("{PARAKEET_BASE}/{SMALLEST_LFS}"),
        &dest,
        &[],
        |_, _| {},
    ));

    let outcome = result.as_ref().err().cloned().unwrap_or_default();
    let size = std::fs::metadata(&dest).map(|m| m.len()).unwrap_or(0);
    let _ = std::fs::remove_dir_all(&dir);

    assert!(result.is_ok(), "download failed: {outcome}");
    assert!(size > 0, "the file was downloaded but is empty");
}

/// The trap itself, pinned: the ETag on the response the redirect leads to is
/// NOT the content hash, yet it is 64 hex characters, so any `is_sha256`
/// filter will happily accept it. Never fall back to it.
#[test]
#[ignore]
fn the_post_redirect_etag_is_not_the_content_hash() {
    let url = format!("{PARAKEET_BASE}/{SMALLEST_LFS}");
    let resp = reqwest::blocking::get(&url).expect("GET the file");
    let cdn_etag = resp
        .headers()
        .get(reqwest::header::ETAG)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim_matches('"').to_string())
        .expect("the CDN response carries an ETag");
    let body = resp.bytes().expect("body");
    let real = dimmy_lib::download::hex_lower(&Sha256::digest(&body));

    assert!(
        dimmy_lib::download::is_sha256(&cdn_etag),
        "precondition: the CDN ETag looks like a sha256 ({cdn_etag})"
    );
    assert_ne!(
        cdn_etag, real,
        "the CDN ETag now equals the content hash — harmless, but the fallback \
         that assumed this is still forbidden: it was false from June to \
         September 2026 and broke the download for 124 users"
    );
}
