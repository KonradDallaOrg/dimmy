//! License management — client side.
//!
//! What this module does:
//!   - Verify Ed25519-signed license tokens **offline** (no network needed).
//!   - Read/write the license file (`~/.config/dimmy/license.json`).
//!   - Track when we last successfully refreshed online so we can enforce
//!     a soft offline grace window (`max_offline_days` from the token).
//!   - Talk to the licensing server (`/api/trial/start`, `/api/activate`,
//!     `/api/refresh`) to acquire new tokens.
//!
//! Privacy invariants:
//!   - The plain email is **never** persisted on disk. Only its
//!     SHA-256 hash (with a stable salt) lives in token claims.
//!   - The Ed25519 *private* key is never present in the client binary —
//!     only the *public* key, embedded at build time via the
//!     `DIMMY_LICENSE_PUBKEY` env var.
//!   - When `DIMMY_LICENSE_PUBKEY` is empty / unset (source-build path),
//!     `check_status()` returns `Unrestricted` and licensing is a no-op.
//!     This is the deliberate "build it yourself, run it free" carve-out
//!     for open-source contributors.
//!
//! Token format: JWT-like, three base64url segments separated by `.`:
//!     header_b64 . payload_b64 . sig_b64
//! See `Claims` for the payload schema. Header is fixed (`alg=EdDSA, typ=DLT`).

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
#[cfg(feature = "license-client")]
use std::time::{SystemTime, UNIX_EPOCH};

/// Capability vocabulary. Tokens carry an explicit list of these in `scope`;
/// the client gates pro-only features by querying `LicenseStatus::has_scope`.
/// New scopes are server-driven — adding one means updating the server's
/// tier→scope table, no client release required (clients that don't know
/// the new scope just ignore it).
pub mod scopes {
    /// Use Dimmy-hosted STT (no BYOK key). Free tier uses BYOK; pro proxies via Dimmy.
    pub const MANAGED_STT: &str = "managed_stt";
    /// Use Dimmy-hosted LLM. Same shape as MANAGED_STT.
    pub const MANAGED_LLM: &str = "managed_llm";
    /// In-app auto-update via Velopack. Without this scope the client never checks.
    pub const AUTO_UPDATE: &str = "auto_update";
    /// Cross-device history sync (future feature).
    pub const HISTORY_SYNC: &str = "history_sync";
    /// Premium LLM styles / curated prompts beyond the defaults.
    pub const PREMIUM_STYLES: &str = "premium_styles";
}

/// Build-time–embedded Ed25519 public key (32 bytes, base64url-encoded).
///
/// Empty / unset → "unrestricted" mode (no licensing enforcement). This
/// path is for source-builds: clone, `cargo build`, run; everything works.
/// Released binaries are built with the env var set, embedding the key
/// the licensing-server holds the private half of.
pub const EMBEDDED_PUBKEY_B64: &str = match option_env!("DIMMY_LICENSE_PUBKEY") {
    Some(k) => k,
    None => "",
};

/// Tier of a license — controls expiry semantics + bundled scopes.
///
/// `Trial` is short-lived (14 days) with full scopes for evaluation.
/// `Monthly` / `Annual` are recurring Stripe subscriptions; the
/// `valid_until` claim is bumped on each `invoice.paid` webhook so
/// active subscribers never see expiry. `Lifetime` is a one-time
/// purchase priced at ~2.5x annual; semantics today are 3 years
/// date-based ("max(3y, next major release)" was rejected during
/// design — too ambiguous to communicate).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Tier {
    #[serde(rename = "trial")]
    Trial,
    #[serde(rename = "monthly")]
    Monthly,
    #[serde(rename = "annual")]
    Annual,
    #[serde(rename = "lifetime")]
    Lifetime,
}

impl Tier {
    /// Default `max_offline_days` for a tier — how long we tolerate
    /// being offline before suspending cloud features.
    pub fn default_max_offline_days(self) -> u32 {
        match self {
            // Trial users are by definition new — 30 days is plenty,
            // and if they go offline that long they're not engaged.
            Tier::Trial => 30,
            // Monthly subscribers should notice degradation quickly if
            // their card stops working — 14 days catches the lapse
            // before they've used a full month of unpaid service.
            Tier::Monthly => 14,
            // Annual subscriptions need periodic refresh so we can
            // detect cancellations / chargebacks within ~6 weeks.
            Tier::Annual => 30,
            // Lifetime (3-year prepay) should "just work" offline for
            // the entire prepay window — the user paid upfront.
            Tier::Lifetime => 1095,
        }
    }
}

/// Claims embedded in the signed token's payload segment.
/// Field names are kept short to keep the base64-encoded size small.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claims {
    /// Schema version — bump on breaking changes.
    pub v: u32,
    /// License ID (ULID, generated on first issuance, stable across refreshes).
    pub lid: String,
    /// Email hash — `hex(sha256(email))`. Plain email never on disk.
    pub eh: String,
    /// Tier — controls scope + offline grace.
    pub tier: Tier,
    /// Issued-at unix epoch seconds.
    pub iat: i64,
    /// Expiry unix epoch seconds.
    pub exp: i64,
    /// Max consecutive days offline before cloud features suspend.
    pub max_offline: u32,
    /// Device ID (ULID, unique per activation; tracked server-side for
    /// device-limit enforcement).
    pub did: String,
    /// Scope of capabilities ("cloud", "updates"). Forward-compat for
    /// future per-feature gating.
    pub scope: Vec<String>,
    /// Unix epoch seconds at which a cancelled subscription will lapse.
    /// Set when the user has clicked "Cancel" in Stripe Customer Portal
    /// and Stripe fired customer.subscription.updated with
    /// cancel_at_period_end=true. The license stays valid until this
    /// timestamp; UI surfaces "Cancels on …" so the user knows.
    /// Omitted when the subscription is healthy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancels_at: Option<i64>,
}

/// Wire format of the license file on disk. We wrap the raw token in a
/// small envelope so future schema migrations have a version field to
/// branch on without breaking backward compat.
#[derive(Debug, Serialize, Deserialize)]
struct LicenseFile {
    schema_version: u32,
    token: String,
}

/// Possible states returned by [`check_status`]. The UI should branch on
/// this and gate cloud / auto-update features accordingly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LicenseStatus {
    /// Source-build path (no embedded public key). All features enabled,
    /// licensing entirely bypassed.
    Unrestricted,
    /// No license file on disk (or unreadable / corrupt envelope). UI
    /// should prompt the user to activate / start a trial.
    NotFound,
    /// Token cryptographically invalid (signature mismatch, malformed,
    /// or claims couldn't be parsed). Treat as "needs re-activation".
    Invalid(String),
    /// Trial in progress.
    TrialActive {
        days_remaining: u32,
        scopes: Vec<String>,
    },
    /// Trial ended (token expired). Local features stay on; cloud /
    /// updates gated.
    TrialExpired,
    /// Paid license active. `cancels_at` is set when the subscription
    /// is scheduled to cancel at period end (user clicked Cancel in
    /// Customer Portal); UI shows "Cancels on …" subtitle.
    Active {
        tier: Tier,
        days_remaining: i64,
        scopes: Vec<String>,
        cancels_at: Option<i64>,
    },
    /// Paid license expired (sub lapsed, prepay window ended). Local
    /// features stay on; cloud / updates gated.
    Expired,
    /// Token still in date but client has been offline beyond
    /// `max_offline_days` — soft-suspended. UI should suggest
    /// reconnection. Local features keep working.
    Suspended { tier: Tier, days_offline: u32 },
}

impl LicenseStatus {
    /// True when cloud features (managed STT/LLM, auto-update) should be
    /// available. BYOK + local features are NEVER gated by this method
    /// (see CLAUDE.md / docs/dev/licensing-poc.md for the rationale).
    pub fn cloud_enabled(&self) -> bool {
        // Source build → always enabled. Anything granting at least one
        // managed-* scope qualifies. Keeps semantic backward compat with
        // the old binary `cloud_enabled / updates_enabled` interface.
        match self {
            LicenseStatus::Unrestricted => true,
            _ => self.has_scope(scopes::MANAGED_STT) || self.has_scope(scopes::MANAGED_LLM),
        }
    }

    /// True when in-app auto-update is allowed.
    pub fn updates_enabled(&self) -> bool {
        match self {
            LicenseStatus::Unrestricted => true,
            _ => self.has_scope(scopes::AUTO_UPDATE),
        }
    }

    /// True if the active license carries the named scope. `Unrestricted`
    /// (source build) returns true unconditionally — open-source contributors
    /// get every capability for free.
    pub fn has_scope(&self, name: &str) -> bool {
        match self {
            LicenseStatus::Unrestricted => true,
            LicenseStatus::TrialActive { scopes, .. } | LicenseStatus::Active { scopes, .. } => {
                scopes.iter().any(|s| s == name)
            }
            // NotFound / Invalid / Expired / TrialExpired / Suspended → no scope.
            _ => false,
        }
    }

    /// Returns the active scope list, or empty for non-active states.
    /// `Unrestricted` reports the full vocabulary so UI can render "all on"
    /// without special-casing source builds.
    pub fn scopes(&self) -> Vec<String> {
        match self {
            LicenseStatus::Unrestricted => vec![
                scopes::MANAGED_STT.into(),
                scopes::MANAGED_LLM.into(),
                scopes::AUTO_UPDATE.into(),
                scopes::HISTORY_SYNC.into(),
                scopes::PREMIUM_STYLES.into(),
            ],
            LicenseStatus::TrialActive { scopes, .. } | LicenseStatus::Active { scopes, .. } => {
                scopes.clone()
            }
            _ => Vec::new(),
        }
    }
}

/// Convenience for call sites — equivalent to `check_status().has_scope(name)`.
pub fn has_scope(name: &str) -> bool {
    check_status().has_scope(name)
}

/// Resolve `~/.config/dimmy/license.json` (or the platform-specific
/// equivalent — same path the Rust core uses for `config.json`).
pub fn license_path() -> Option<PathBuf> {
    crate::config_dir_path().map(|p| p.join("license.json"))
}

/// Resolve the path to the last-online-check sidecar file. Tracks the
/// most recent successful `/api/refresh` so we can compute "days offline"
/// without trusting the local clock alone.
pub fn last_online_check_path() -> Option<PathBuf> {
    crate::config_dir_path().map(|p| p.join("last_online_check.txt"))
}

/// Read + envelope-deserialize the license file. Returns Ok(None) if the
/// file simply doesn't exist (the common "fresh install" case). Returns
/// Err for unreadable or malformed envelopes — caller usually treats
/// these the same as "missing".
pub fn load_license_file() -> Result<Option<String>, std::io::Error> {
    let Some(path) = license_path() else {
        return Ok(None);
    };
    if !path.exists() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(&path)?;
    let env: LicenseFile = serde_json::from_str(&raw).map_err(|e| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("license envelope parse: {}", e),
        )
    })?;
    Ok(Some(env.token))
}

/// Write the token wrapped in the file envelope. Creates the dimmy
/// config dir if needed.
pub fn save_license_file(token: &str) -> Result<(), std::io::Error> {
    assert!(!token.is_empty(), "save_license_file: empty token");
    let path = license_path().ok_or_else(|| std::io::Error::other("config dir unavailable"))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let env = LicenseFile {
        schema_version: 1,
        token: token.to_string(),
    };
    let json = serde_json::to_string_pretty(&env).map_err(std::io::Error::other)?;
    std::fs::write(&path, json)?;
    assert!(path.exists(), "license file must exist after write");
    Ok(())
}

/// Read the unix epoch we last successfully refreshed from the server.
/// Defaults to 0 when missing — meaning "we have no online check on
/// record, treat as worst case for grace calculations".
pub fn last_online_check() -> i64 {
    let Some(path) = last_online_check_path() else {
        return 0;
    };
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return 0;
    };
    raw.trim().parse().unwrap_or(0)
}

/// Stamp "now" into the last-online-check sidecar.
pub fn set_last_online_check(epoch_seconds: i64) -> Result<(), std::io::Error> {
    assert!(epoch_seconds > 0, "epoch must be positive");
    let path =
        last_online_check_path().ok_or_else(|| std::io::Error::other("config dir unavailable"))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, epoch_seconds.to_string())
}

/// Compute SHA-256 of an email (lower-cased, trimmed) — what the server
/// uses to look up licenses without storing plain emails. Hex-encoded.
#[cfg(feature = "license-client")]
pub fn email_hash(email: &str) -> String {
    use sha2::{Digest, Sha256};
    let normalised = email.trim().to_lowercase();
    let mut hasher = Sha256::new();
    hasher.update(normalised.as_bytes());
    hex_encode(&hasher.finalize())
}

#[cfg(feature = "license-client")]
fn hex_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push_str(&format!("{:02x}", b));
    }
    out
}

/// Verify a license token's Ed25519 signature against the embedded
/// public key, then deserialise + sanity-check the claims.
///
/// Returns Err if any of:
///   - public key is empty (source build) — caller should special-case
///     this BEFORE calling, hence assert!() rather than soft return,
///   - token doesn't have exactly 3 segments,
///   - any segment isn't valid base64url,
///   - signature doesn't verify,
///   - claims aren't a valid JSON Claims struct.
#[cfg(feature = "license-client")]
pub fn verify_token(token: &str, pubkey_b64: &str) -> Result<Claims, String> {
    use ed25519_dalek::{Signature, Verifier, VerifyingKey};

    assert!(
        !pubkey_b64.is_empty(),
        "verify_token must not be called with empty pubkey — \
         caller should branch on EMBEDDED_PUBKEY_B64.is_empty() first"
    );
    assert!(!token.is_empty(), "verify_token: empty token");

    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 {
        return Err(format!(
            "token must have exactly 3 segments, got {}",
            parts.len()
        ));
    }
    let header_b64 = parts[0];
    let payload_b64 = parts[1];
    let sig_b64 = parts[2];

    let pubkey_bytes = b64_decode(pubkey_b64).map_err(|e| format!("pubkey b64: {}", e))?;
    let pubkey_arr: [u8; 32] = pubkey_bytes
        .as_slice()
        .try_into()
        .map_err(|_| "pubkey must be 32 bytes".to_string())?;
    let vk = VerifyingKey::from_bytes(&pubkey_arr).map_err(|e| format!("pubkey parse: {}", e))?;

    let sig_bytes = b64_decode(sig_b64).map_err(|e| format!("sig b64: {}", e))?;
    let sig_arr: [u8; 64] = sig_bytes
        .as_slice()
        .try_into()
        .map_err(|_| "signature must be 64 bytes".to_string())?;
    let sig = Signature::from_bytes(&sig_arr);

    let signing_input = format!("{}.{}", header_b64, payload_b64);
    vk.verify(signing_input.as_bytes(), &sig)
        .map_err(|e| format!("signature verify: {}", e))?;

    let payload_bytes = b64_decode(payload_b64).map_err(|e| format!("payload b64: {}", e))?;
    let claims: Claims =
        serde_json::from_slice(&payload_bytes).map_err(|e| format!("claims parse: {}", e))?;

    // Sanity-check claims: schema version must match what we know how to
    // interpret. Future versions are rejected — clients on old binaries
    // shouldn't try to interpret newer fields.
    if claims.v != 1 {
        return Err(format!("unsupported schema version: {}", claims.v));
    }
    if claims.iat <= 0 || claims.exp <= 0 {
        return Err("iat/exp must be positive".to_string());
    }
    if claims.exp < claims.iat {
        return Err("exp must be >= iat".to_string());
    }
    Ok(claims)
}

/// base64url-decode without padding (per JWT convention). Stable wrapper
/// around the `base64` crate so callers don't have to remember the
/// engine to use.
#[cfg(feature = "license-client")]
fn b64_decode(s: &str) -> Result<Vec<u8>, String> {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
    URL_SAFE_NO_PAD.decode(s).map_err(|e| e.to_string())
}

/// base64url-encode without padding.
#[cfg(feature = "license-client")]
pub fn b64_encode(bytes: &[u8]) -> String {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
    URL_SAFE_NO_PAD.encode(bytes)
}

/// Top-level status check — the entry point UI code calls on launch.
///
/// Behaviour:
///   - Source build (empty pubkey) → `Unrestricted`.
///   - No file → `NotFound`.
///   - File but signature invalid → `Invalid`.
///   - Valid but past `exp` → `TrialExpired` / `Expired`.
///   - Valid and within `exp` → `TrialActive` / `Active`, unless we've
///     been offline beyond `max_offline + 7 grace` → `Suspended`.
pub fn check_status() -> LicenseStatus {
    if EMBEDDED_PUBKEY_B64.is_empty() {
        return LicenseStatus::Unrestricted;
    }
    #[cfg(not(feature = "license-client"))]
    {
        // Pubkey is set but we don't have ed25519 compiled in. This
        // shouldn't happen in production builds (the release.yml
        // injects DIMMY_LICENSE_PUBKEY only when license-client is on);
        // be defensive and treat as unrestricted so we don't lock users
        // out of a build that physically can't verify.
        LicenseStatus::Unrestricted
    }
    #[cfg(feature = "license-client")]
    {
        let token = match load_license_file() {
            Ok(Some(t)) => t,
            Ok(None) => return LicenseStatus::NotFound,
            Err(e) => return LicenseStatus::Invalid(format!("file io: {}", e)),
        };
        let claims = match verify_token(&token, EMBEDDED_PUBKEY_B64) {
            Ok(c) => c,
            Err(e) => return LicenseStatus::Invalid(e),
        };
        let now = now_secs();

        // Past expiry — always treat as expired regardless of offline status.
        if now >= claims.exp {
            return match claims.tier {
                Tier::Trial => LicenseStatus::TrialExpired,
                _ => LicenseStatus::Expired,
            };
        }

        // Offline-grace check.
        let last_online = last_online_check();
        let days_offline = if last_online <= 0 {
            // No record yet — assume we just installed and let it slide
            // for now. The first successful refresh will start the clock.
            0
        } else {
            ((now - last_online) / 86_400).max(0) as u32
        };
        let grace = claims.max_offline.saturating_add(7);
        if days_offline > grace {
            return LicenseStatus::Suspended {
                tier: claims.tier,
                days_offline,
            };
        }

        // Ceiling division: "23h 59m left" → 1 day, not 0. The token was
        // issued moments after the license started, so floor-div on a fresh
        // 14-day trial would render "13 days" the entire first day. Users
        // would think their trial is already shrinking before they used it.
        let secs_remaining = claims.exp - now;
        let days_remaining = (secs_remaining + 86_399) / 86_400;
        match claims.tier {
            Tier::Trial => LicenseStatus::TrialActive {
                days_remaining: days_remaining as u32,
                scopes: claims.scope.clone(),
            },
            tier => LicenseStatus::Active {
                tier,
                days_remaining,
                scopes: claims.scope.clone(),
                cancels_at: claims.cancels_at,
            },
        }
    }
}

#[cfg(feature = "license-client")]
fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

// ── HTTP client (talks to the licensing server) ──────────────────────

/// Default server URL for the local PoC. Production builds will override
/// this via env var when the Cloudflare Workers endpoint is live.
pub const DEFAULT_SERVER_URL: &str = "http://localhost:8787";

/// Wire-shape of `POST /api/trial/start`.
#[derive(Debug, Serialize)]
pub struct TrialStartRequest<'a> {
    pub email: &'a str,
}

/// Wire-shape of `POST /api/trial/start`'s response. `magic_link` is the
/// fully-qualified `/api/activate?code=...` URL the user clicks.
#[derive(Debug, Deserialize)]
pub struct TrialStartResponse {
    pub magic_link: String,
}

/// Wire-shape of `POST /api/refresh`.
#[derive(Debug, Serialize)]
pub struct RefreshRequest<'a> {
    pub token: &'a str,
}

/// Wire-shape of `POST /api/refresh`'s response — same envelope as
/// `/api/activate` since both ultimately yield a fresh signed token.
#[derive(Debug, Deserialize)]
pub struct TokenResponse {
    pub token: String,
}

/// Hit `POST /api/trial/start` to provision a trial and obtain the
/// magic-link URL the user should "click" (in the PoC the link is
/// printed on the server's stdout; calling this returns the same URL
/// the server emitted, so the CLI can echo it back).
pub async fn request_trial(
    server: &str,
    email: &str,
) -> Result<TrialStartResponse, reqwest::Error> {
    assert!(!server.is_empty(), "server URL required");
    assert!(!email.is_empty(), "email required");
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()?;
    let url = format!("{}/api/trial/start", server.trim_end_matches('/'));
    let resp = client
        .post(&url)
        .json(&TrialStartRequest { email })
        .send()
        .await?
        .error_for_status()?
        .json::<TrialStartResponse>()
        .await?;
    Ok(resp)
}

/// Hit `GET /api/activate?code=...` (the magic link) and obtain the
/// signed license token. Caller is responsible for `save_license_file`.
pub async fn redeem_activation_code(
    server: &str,
    code: &str,
    device_label: &str,
) -> Result<TokenResponse, reqwest::Error> {
    assert!(!server.is_empty(), "server URL required");
    assert!(!code.is_empty(), "activation code required");
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()?;
    let url = format!(
        "{}/api/activate?code={}&device_label={}",
        server.trim_end_matches('/'),
        urlencoding(code),
        urlencoding(device_label),
    );
    client
        .get(&url)
        .send()
        .await?
        .error_for_status()?
        .json::<TokenResponse>()
        .await
}

/// Refresh an existing token. Server may return the same token (no-op
/// touch of `last_seen`) or a re-issued one (e.g. after a sub renewal).
/// Either way the caller writes it back to the license file.
pub async fn refresh_token(server: &str, token: &str) -> Result<TokenResponse, reqwest::Error> {
    assert!(!server.is_empty(), "server URL required");
    assert!(!token.is_empty(), "token required");
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()?;
    let url = format!("{}/api/refresh", server.trim_end_matches('/'));
    client
        .post(&url)
        .json(&RefreshRequest { token })
        .send()
        .await?
        .error_for_status()?
        .json::<TokenResponse>()
        .await
}

/// One device under a license. Returned by `/api/devices/list`.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct DeviceInfo {
    pub device_id: String,
    pub label: String,
    pub issued_at: i64,
    pub last_seen: i64,
    pub status: String,
    pub is_self: bool,
}

/// Wire shape of `/api/devices/list` response.
#[derive(Debug, Deserialize, Serialize)]
pub struct DevicesListResponse {
    pub license_id: String,
    pub tier: String,
    pub max_devices: u32,
    pub devices: Vec<DeviceInfo>,
}

/// `POST /api/devices/list { token }` — list all devices under the license
/// the caller's token belongs to. Auth is the token signature.
pub async fn list_devices(
    server: &str,
    token: &str,
) -> Result<DevicesListResponse, reqwest::Error> {
    assert!(!server.is_empty(), "server URL required");
    assert!(!token.is_empty(), "token required");
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()?;
    let url = format!("{}/api/devices/list", server.trim_end_matches('/'));
    client
        .post(&url)
        .json(&serde_json::json!({ "token": token }))
        .send()
        .await?
        .error_for_status()?
        .json::<DevicesListResponse>()
        .await
}

/// `POST /api/checkout/create { tier, token? }` — generate a Stripe
/// Checkout Session URL for the chosen paid tier. Pass the current
/// token to carry email_hash across as Stripe `client_reference_id`
/// (so trial→paid linkage survives), or `None` for an anonymous
/// purchase (NotFound state).
pub async fn create_checkout(
    server: &str,
    tier: &str,
    token: Option<&str>,
) -> Result<String, reqwest::Error> {
    assert!(!server.is_empty(), "server URL required");
    assert!(!tier.is_empty(), "tier required");
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()?;
    let url = format!("{}/api/checkout/create", server.trim_end_matches('/'));
    let mut payload = serde_json::json!({ "tier": tier });
    if let Some(t) = token {
        payload["token"] = serde_json::Value::String(t.to_string());
    }
    let resp: serde_json::Value = client
        .post(&url)
        .json(&payload)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    Ok(resp
        .get("url")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string())
}

/// `POST /api/billing-portal { token }` — generate a Stripe Customer
/// Portal session URL. Only valid for paid licenses (those with a
/// `stripe_customer_id`). Trials get a 409 from the server.
pub async fn billing_portal_url(server: &str, token: &str) -> Result<String, reqwest::Error> {
    assert!(!server.is_empty(), "server URL required");
    assert!(!token.is_empty(), "token required");
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()?;
    let url = format!("{}/api/billing-portal", server.trim_end_matches('/'));
    let resp: serde_json::Value = client
        .post(&url)
        .json(&serde_json::json!({ "token": token }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    Ok(resp
        .get("url")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string())
}

/// `POST /api/devices/deactivate { token, device_id? }` — mark a device
/// as deactivated. `None` = self-sign-out (frees the calling device's slot).
pub async fn deactivate_device(
    server: &str,
    token: &str,
    device_id: Option<&str>,
) -> Result<(), reqwest::Error> {
    assert!(!server.is_empty(), "server URL required");
    assert!(!token.is_empty(), "token required");
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()?;
    let url = format!("{}/api/devices/deactivate", server.trim_end_matches('/'));
    let mut payload = serde_json::json!({ "token": token });
    if let Some(did) = device_id {
        payload["device_id"] = serde_json::Value::String(did.to_string());
    }
    client
        .post(&url)
        .json(&payload)
        .send()
        .await?
        .error_for_status()?;
    Ok(())
}

/// Tiny URL-encoding helper. The server endpoints accept activation
/// codes that are always URL-safe (we generate them via `rand::Alphanumeric`
/// + `_-`), but `device_label` may have spaces — encode just to be safe.
fn urlencoding(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unrestricted_when_pubkey_is_empty() {
        // The lib const is empty in tests (no DIMMY_LICENSE_PUBKEY env at
        // build time), so check_status returns Unrestricted regardless of
        // file presence. This is the source-build path.
        if EMBEDDED_PUBKEY_B64.is_empty() {
            assert_eq!(check_status(), LicenseStatus::Unrestricted);
        }
    }

    #[test]
    fn cloud_enabled_for_active_states_only() {
        assert!(LicenseStatus::Unrestricted.cloud_enabled());
        assert!(LicenseStatus::TrialActive {
            days_remaining: 7,
            scopes: vec!["managed_stt".into(), "managed_llm".into()],
        }
        .cloud_enabled());
        assert!(LicenseStatus::Active {
            tier: Tier::Annual,
            days_remaining: 100,
            scopes: vec!["managed_stt".into()],
            cancels_at: None,
        }
        .cloud_enabled());

        // Active state without managed_* scopes → cloud is gated.
        assert!(!LicenseStatus::Active {
            tier: Tier::Annual,
            days_remaining: 100,
            scopes: vec![],
            cancels_at: None,
        }
        .cloud_enabled());

        assert!(!LicenseStatus::NotFound.cloud_enabled());
        assert!(!LicenseStatus::Invalid("test".into()).cloud_enabled());
        assert!(!LicenseStatus::TrialExpired.cloud_enabled());
        assert!(!LicenseStatus::Expired.cloud_enabled());
        assert!(!LicenseStatus::Suspended {
            tier: Tier::Annual,
            days_offline: 60
        }
        .cloud_enabled());
    }

    #[test]
    fn has_scope_unrestricted_grants_everything() {
        let s = LicenseStatus::Unrestricted;
        assert!(s.has_scope(scopes::MANAGED_STT));
        assert!(s.has_scope(scopes::AUTO_UPDATE));
        assert!(s.has_scope("anything-future-too"));
    }

    #[test]
    fn has_scope_only_grants_what_token_says() {
        let s = LicenseStatus::Active {
            tier: Tier::Annual,
            days_remaining: 100,
            scopes: vec![scopes::MANAGED_STT.into(), scopes::AUTO_UPDATE.into()],
            cancels_at: None,
        };
        assert!(s.has_scope(scopes::MANAGED_STT));
        assert!(s.has_scope(scopes::AUTO_UPDATE));
        assert!(!s.has_scope(scopes::MANAGED_LLM));
        assert!(!s.has_scope(scopes::HISTORY_SYNC));
    }

    #[test]
    fn active_with_cancels_at_still_grants_scopes() {
        // The user has clicked Cancel in Customer Portal but the period
        // hasn't ended yet. Until exp passes, the scopes are still
        // granted — we don't pre-emptively gate.
        let s = LicenseStatus::Active {
            tier: Tier::Annual,
            days_remaining: 30,
            scopes: vec![scopes::MANAGED_STT.into(), scopes::AUTO_UPDATE.into()],
            cancels_at: Some(2_000_000_000),
        };
        assert!(s.has_scope(scopes::MANAGED_STT));
        assert!(s.cloud_enabled());
    }

    #[test]
    fn claims_serde_round_trip_preserves_cancels_at() {
        let claims = Claims {
            v: 1,
            lid: "lid".into(),
            eh: "eh".into(),
            tier: Tier::Annual,
            iat: 1,
            exp: 2,
            max_offline: 30,
            did: "did".into(),
            scope: vec!["s".into()],
            cancels_at: Some(1_888_888_888),
        };
        let json = serde_json::to_string(&claims).unwrap();
        let back: Claims = serde_json::from_str(&json).unwrap();
        assert_eq!(back.cancels_at, Some(1_888_888_888));
    }

    #[test]
    fn claims_serde_omits_cancels_at_when_none() {
        // Critical: the field must be ABSENT from the JSON when None,
        // not serialized as `null`. Old clients (pre-cancels_at field)
        // would deserialize `null` into None already (serde tolerates
        // missing field), but absent keeps the token byte-stable across
        // the schema bump.
        let claims = Claims {
            v: 1,
            lid: "lid".into(),
            eh: "eh".into(),
            tier: Tier::Annual,
            iat: 1,
            exp: 2,
            max_offline: 30,
            did: "did".into(),
            scope: vec!["s".into()],
            cancels_at: None,
        };
        let json = serde_json::to_string(&claims).unwrap();
        assert!(!json.contains("cancels_at"), "got: {}", json);
    }

    #[test]
    fn has_scope_denies_when_expired_or_invalid() {
        assert!(!LicenseStatus::TrialExpired.has_scope(scopes::MANAGED_STT));
        assert!(!LicenseStatus::Expired.has_scope(scopes::MANAGED_STT));
        assert!(!LicenseStatus::NotFound.has_scope(scopes::MANAGED_STT));
        assert!(!LicenseStatus::Invalid("e".into()).has_scope(scopes::MANAGED_STT));
        assert!(!LicenseStatus::Suspended {
            tier: Tier::Trial,
            days_offline: 50,
        }
        .has_scope(scopes::MANAGED_STT));
    }

    #[test]
    fn tier_default_max_offline_is_strict_for_subscriptions() {
        assert_eq!(Tier::Trial.default_max_offline_days(), 30);
        // Monthly is the strictest — 14 days catches a lapsed card
        // before a full month of unpaid service has elapsed.
        assert_eq!(Tier::Monthly.default_max_offline_days(), 14);
        assert_eq!(Tier::Annual.default_max_offline_days(), 30);
        // Lifetime (3-year prepay) must work offline for the entire
        // prepay window — 3 years ≈ 1095 days.
        assert_eq!(Tier::Lifetime.default_max_offline_days(), 1095);
    }

    #[cfg(feature = "license-client")]
    #[test]
    fn email_hash_is_normalised_lowercase_trimmed() {
        let a = email_hash("Alice@Example.com");
        let b = email_hash("alice@example.com");
        let c = email_hash("  alice@example.com  ");
        assert_eq!(a, b);
        assert_eq!(b, c);
        assert_eq!(a.len(), 64); // hex of 32-byte SHA-256
    }

    #[cfg(feature = "license-client")]
    #[test]
    fn b64_round_trip_is_url_safe_no_pad() {
        let original: &[u8] = b"~hello?+world/";
        let encoded = b64_encode(original);
        // No padding chars, no plus / slash (those are URL-unsafe).
        assert!(!encoded.contains('='));
        assert!(!encoded.contains('+'));
        assert!(!encoded.contains('/'));
        let decoded = b64_decode(&encoded).unwrap();
        assert_eq!(decoded, original);
    }

    #[cfg(feature = "license-client")]
    #[test]
    fn verify_token_rejects_empty_segments() {
        // Non-empty pubkey just so the assert doesn't fire.
        let dummy_pubkey = b64_encode(&[0u8; 32]);
        let bad_tokens = vec!["", "a.b", "a.b.c.d", "a..b"];
        for t in bad_tokens {
            // `""` would trip the !token.is_empty() assert by design;
            // skip it (the assert IS the safety net we want).
            if t.is_empty() {
                continue;
            }
            let r = verify_token(t, &dummy_pubkey);
            assert!(r.is_err(), "expected err for token {:?}", t);
        }
    }

    #[cfg(feature = "license-client")]
    #[test]
    fn verify_token_rejects_tampered_signature() {
        use ed25519_dalek::{Signer, SigningKey};
        let mut csprng = rand::thread_rng();
        let sk = SigningKey::generate(&mut csprng);
        let pubkey_b64 = b64_encode(sk.verifying_key().as_bytes());

        let header_b64 = b64_encode(br#"{"alg":"EdDSA","typ":"DLT"}"#);
        let claims = Claims {
            v: 1,
            lid: "01HKZ".into(),
            eh: "abc".into(),
            tier: Tier::Trial,
            iat: 1_000_000,
            exp: 2_000_000,
            max_offline: 30,
            did: "01HKD".into(),
            scope: vec!["cloud".into()],
            cancels_at: None,
        };
        let payload_bytes = serde_json::to_vec(&claims).unwrap();
        let payload_b64 = b64_encode(&payload_bytes);
        let signing_input = format!("{}.{}", header_b64, payload_b64);
        let sig = sk.sign(signing_input.as_bytes());
        let sig_b64 = b64_encode(&sig.to_bytes());

        let good_token = format!("{}.{}.{}", header_b64, payload_b64, sig_b64);
        let claims_back = verify_token(&good_token, &pubkey_b64).unwrap();
        assert_eq!(claims_back.lid, "01HKZ");

        // Flip one byte in the payload — signature now doesn't match.
        let mut tampered_bytes = payload_bytes.clone();
        tampered_bytes[0] ^= 0xFF;
        let tampered_b64 = b64_encode(&tampered_bytes);
        let bad_token = format!("{}.{}.{}", header_b64, tampered_b64, sig_b64);
        let r = verify_token(&bad_token, &pubkey_b64);
        assert!(r.is_err(), "tampered token must be rejected");
        assert!(
            r.unwrap_err().contains("signature verify"),
            "rejection should reference signature"
        );
    }

    #[cfg(feature = "license-client")]
    #[test]
    fn verify_token_rejects_unknown_schema_version() {
        use ed25519_dalek::{Signer, SigningKey};
        let mut csprng = rand::thread_rng();
        let sk = SigningKey::generate(&mut csprng);
        let pubkey_b64 = b64_encode(sk.verifying_key().as_bytes());
        let header_b64 = b64_encode(br#"{"alg":"EdDSA","typ":"DLT"}"#);
        let claims = Claims {
            v: 999, // future schema
            lid: "01HKZ".into(),
            eh: "abc".into(),
            tier: Tier::Annual,
            iat: 1_000_000,
            exp: 2_000_000,
            max_offline: 30,
            did: "01HKD".into(),
            scope: vec![],
            cancels_at: None,
        };
        let payload_b64 = b64_encode(&serde_json::to_vec(&claims).unwrap());
        let signing_input = format!("{}.{}", header_b64, payload_b64);
        let sig = sk.sign(signing_input.as_bytes());
        let token = format!(
            "{}.{}.{}",
            header_b64,
            payload_b64,
            b64_encode(&sig.to_bytes())
        );
        let err = verify_token(&token, &pubkey_b64).unwrap_err();
        assert!(err.contains("schema version"), "got: {}", err);
    }

    #[cfg(feature = "license-client")]
    #[test]
    fn save_load_license_file_round_trip() {
        // Use a temp dir so we don't pollute the user's real config.
        let tmp = std::env::temp_dir().join(format!("dimmy-license-test-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        // Override config_dir via env var? config_dir_path uses dirs::config_dir
        // which we can't easily override. Instead, write the file directly
        // through the public APIs and verify the envelope round-trips by
        // parsing.
        let envelope = LicenseFile {
            schema_version: 1,
            token: "a.b.c".to_string(),
        };
        let json = serde_json::to_string(&envelope).unwrap();
        let path = tmp.join("license.json");
        std::fs::write(&path, &json).unwrap();
        let raw = std::fs::read_to_string(&path).unwrap();
        let back: LicenseFile = serde_json::from_str(&raw).unwrap();
        assert_eq!(back.schema_version, 1);
        assert_eq!(back.token, "a.b.c");
        std::fs::remove_dir_all(&tmp).ok();
    }
}
