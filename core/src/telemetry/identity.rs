//! Anonymous identifier for telemetry.
//!
//! A single random UUID per installation, persisted to
//! `~/.config/dimmy/analytics_id` (separate from `config.json` so a
//! settings reset doesn't churn the ID). Resettable from the UI.
//!
//! The ID is **never** linked to anything user-identifiable. It exists
//! only so we can de-duplicate "1 user did X 5 times" from "5 users
//! each did X once".

use std::path::PathBuf;
use std::sync::OnceLock;

/// File name used to persist the anonymous ID.
const ID_FILENAME: &str = "analytics_id";

/// Cached so we don't hit disk on every event.
static CACHED_ID: OnceLock<String> = OnceLock::new();

fn id_path() -> Option<PathBuf> {
    crate::config_dir_path().map(|p| p.join(ID_FILENAME))
}

/// Generate a new UUIDv4 as a 36-char hyphenated string.
fn generate() -> String {
    // Inline UUIDv4 generation to avoid pulling a uuid crate just for this.
    // 16 random bytes, then layout per RFC 4122 §4.4.
    let mut bytes = [0u8; 16];
    getrandom_bytes(&mut bytes);
    bytes[6] = (bytes[6] & 0x0F) | 0x40; // version 4
    bytes[8] = (bytes[8] & 0x3F) | 0x80; // variant 1

    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0], bytes[1], bytes[2], bytes[3],
        bytes[4], bytes[5],
        bytes[6], bytes[7],
        bytes[8], bytes[9],
        bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15],
    )
}

/// Fill `out` with random bytes from the OS RNG.
///
/// We avoid the `getrandom` crate here because Dimmy already bundles
/// many native deps; for 16 bytes the platform-specific calls are
/// trivial and don't need a wrapper.
fn getrandom_bytes(out: &mut [u8]) {
    #[cfg(target_os = "windows")]
    unsafe {
        // BCryptGenRandom is the modern Windows CSPRNG.
        // BCRYPT_USE_SYSTEM_PREFERRED_RNG = 0x00000002
        extern "system" {
            fn BCryptGenRandom(
                hAlgorithm: *mut std::ffi::c_void,
                pbBuffer: *mut u8,
                cbBuffer: u32,
                dwFlags: u32,
            ) -> i32;
        }
        let rc = BCryptGenRandom(std::ptr::null_mut(), out.as_mut_ptr(), out.len() as u32, 2);
        assert_eq!(rc, 0, "BCryptGenRandom failed: 0x{:x}", rc);
    }
    #[cfg(unix)]
    {
        use std::io::Read;
        let mut f = std::fs::File::open("/dev/urandom").expect("open /dev/urandom");
        f.read_exact(out).expect("read /dev/urandom");
    }
}

/// Return the anonymous ID, generating + persisting one on first call.
///
/// Cached after the first call so repeated lookups don't touch disk.
/// If the persistence path is unavailable (e.g. config_dir() is None)
/// we still return a stable in-memory ID for the session.
pub fn anonymous_id() -> &'static str {
    CACHED_ID.get_or_init(|| {
        if let Some(path) = id_path() {
            // Try to read existing.
            if let Ok(existing) = std::fs::read_to_string(&path) {
                let trimmed = existing.trim();
                if is_valid_uuid(trimmed) {
                    return trimmed.to_string();
                }
            }

            // Generate + persist.
            let id = generate();
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::write(&path, &id);
            id
        } else {
            // No persistent path — session-local only.
            generate()
        }
    })
}

/// Forget the persisted ID. The next call to `anonymous_id()` will
/// generate a fresh one. Callable from the FFI as the user's "reset"
/// action.
///
/// Note: because `CACHED_ID` is initialised once per process, the
/// running process keeps using the old value until restart. We
/// document this in the UI ("Restart Dimmy to apply").
pub fn reset() {
    if let Some(path) = id_path() {
        let _ = std::fs::remove_file(path);
    }
}

/// Generate a fresh UUIDv4. Public wrapper used by other telemetry
/// modules that need a one-shot session-scoped identifier (e.g.
/// `session_id` in the PostHog client) without persisting it.
pub fn new_uuid_v4() -> String {
    generate()
}

fn is_valid_uuid(s: &str) -> bool {
    s.len() == 36
        && s.chars().enumerate().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => c == '-',
            _ => c.is_ascii_hexdigit(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_ids_are_valid_uuid_v4_format() {
        for _ in 0..32 {
            let id = generate();
            assert_eq!(id.len(), 36);
            assert!(is_valid_uuid(&id), "not a valid UUID: {}", id);
            // Version nibble at index 14 must be '4'.
            assert_eq!(id.as_bytes()[14], b'4');
            // Variant nibble at index 19 must be 8/9/a/b.
            assert!(matches!(id.as_bytes()[19], b'8' | b'9' | b'a' | b'b'));
        }
    }

    #[test]
    fn generated_ids_are_unique() {
        let a = generate();
        let b = generate();
        assert_ne!(a, b);
    }

    #[test]
    fn is_valid_uuid_accepts_canonical_form() {
        assert!(is_valid_uuid("550e8400-e29b-41d4-a716-446655440000"));
        assert!(!is_valid_uuid("not a uuid"));
        assert!(!is_valid_uuid("550e8400e29b41d4a716446655440000")); // no hyphens
        assert!(!is_valid_uuid("")); // empty
    }
}
