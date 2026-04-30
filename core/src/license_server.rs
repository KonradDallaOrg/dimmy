//! Local-only licensing server — Cloudflare Workers replacement for dev.
//!
//! Stands up a tiny HTTP service (axum) backed by SQLite (sqlx) that
//! issues, refreshes, and revokes signed Ed25519 license tokens.
//! Mirrors what the production Cloudflare Worker will do when this
//! architecture graduates from PoC; the wire shapes + DB schema are
//! 1:1 so the migration is mechanical.
//!
//! Email "delivery" in this build prints the magic-link URL to stdout
//! instead of calling Resend — the human running the server reads it
//! and pastes it. Same UX as production, just no SMTP hop.
//!
//! Boot:
//!   - generate Ed25519 keypair to `data/keys.bin` if missing,
//!   - migrate SQLite schema (idempotent),
//!   - print public key to stdout for embedding in client builds via
//!     `DIMMY_LICENSE_PUBKEY=…`,
//!   - bind 0.0.0.0:PORT (default 8787).
//!
//! See `docs/dev/licensing-poc.md` for the full architecture + the 7
//! end-to-end test scenarios.
//!
//! The module is gated on the `licensing-server` feature in lib.rs;
//! no inner `cfg` attribute needed here.

use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use ed25519_dalek::{Signer, SigningKey, VerifyingKey};
use rand::rngs::OsRng;
use rand::Rng;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;
use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::license::{Claims, Tier};

/// Server state shared between handlers. `Arc` because axum clones it
/// per request.
#[derive(Clone)]
pub struct AppState {
    pub db: SqlitePool,
    pub signing_key: Arc<SigningKey>,
    pub public_url: String,
}

/// Tier-specific token validity in seconds. Trial is 14 days, annual
/// 365, three-year ~1095. Mirrored as a fn rather than a const so we
/// can A/B these later without code duplication.
fn token_validity_secs(tier: Tier) -> i64 {
    match tier {
        Tier::Trial => 14 * 86_400,
        Tier::Annual => 365 * 86_400,
        Tier::ThreeYear => 1095 * 86_400,
    }
}

/// Activation codes (the bit inside the magic link) are short-lived
/// and one-time-use. 10 minutes is plenty — the user clicks the link
/// from email moments after requesting it.
const ACTIVATION_CODE_TTL_SECS: i64 = 600;

/// Top-level entry point. Boots the DB, generates / loads the keypair,
/// builds the axum app, and binds. Runs until SIGINT.
pub async fn serve(bind_addr: &str, data_dir: &Path, public_url: &str) -> anyhow::Result<()> {
    assert!(!bind_addr.is_empty(), "bind_addr required");
    assert!(!public_url.is_empty(), "public_url required");
    std::fs::create_dir_all(data_dir)?;

    // ── Keypair ────────────────────────────────────────────────────
    let key_path = data_dir.join("keys.bin");
    let signing_key = if key_path.exists() {
        let bytes = std::fs::read(&key_path)?;
        if bytes.len() != 32 {
            anyhow::bail!("keys.bin must be exactly 32 bytes");
        }
        let arr: [u8; 32] = bytes
            .as_slice()
            .try_into()
            .map_err(|_| anyhow::anyhow!("keys.bin slice→array"))?;
        SigningKey::from_bytes(&arr)
    } else {
        let mut csprng = OsRng;
        let sk = SigningKey::generate(&mut csprng);
        std::fs::write(&key_path, sk.to_bytes())?;
        // Set strict permissions on Unix so the keypair isn't world-readable.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = std::fs::Permissions::from_mode(0o600);
            let _ = std::fs::set_permissions(&key_path, perms);
        }
        sk
    };

    let pubkey_bytes = signing_key.verifying_key().to_bytes();
    let pubkey_b64 = b64_url(&pubkey_bytes);
    println!("──────────────────────────────────────────────────────────────");
    println!("[licensing-server] Ed25519 public key (embed in client):");
    println!("    DIMMY_LICENSE_PUBKEY={}", pubkey_b64);
    println!("──────────────────────────────────────────────────────────────");

    // ── Database ────────────────────────────────────────────────────
    let db_path = data_dir.join("licensing.db");
    let opts = SqliteConnectOptions::new()
        .filename(&db_path)
        .create_if_missing(true)
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
        .synchronous(sqlx::sqlite::SqliteSynchronous::Normal);
    let db = SqlitePoolOptions::new()
        .max_connections(8)
        .connect_with(opts)
        .await?;
    migrate(&db).await?;

    let state = AppState {
        db,
        signing_key: Arc::new(signing_key),
        public_url: public_url.trim_end_matches('/').to_string(),
    };

    // ── Routes ──────────────────────────────────────────────────────
    let app = Router::new()
        .route("/api/health", get(health))
        .route("/api/trial/start", post(trial_start))
        .route("/api/activate", get(activate))
        .route("/api/refresh", post(refresh))
        .route("/api/license/issue", post(license_issue))
        .route("/api/license/status", get(license_status_debug))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(bind_addr).await?;
    println!("[licensing-server] listening on http://{}", bind_addr);
    println!("[licensing-server] API base: {}", public_url);
    println!("[licensing-server] data dir: {}", data_dir.display());
    axum::serve(listener, app).await?;
    Ok(())
}

async fn migrate(db: &SqlitePool) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS licenses (
            license_id    TEXT PRIMARY KEY,
            email_hash    TEXT NOT NULL,
            tier          TEXT NOT NULL,
            issued_at     INTEGER NOT NULL,
            valid_until   INTEGER NOT NULL,
            max_devices   INTEGER NOT NULL DEFAULT 5,
            status        TEXT NOT NULL DEFAULT 'active'
        )
        "#,
    )
    .execute(db)
    .await?;
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_licenses_email ON licenses(email_hash)")
        .execute(db)
        .await?;

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS devices (
            device_id     TEXT PRIMARY KEY,
            license_id    TEXT NOT NULL,
            device_label  TEXT NOT NULL,
            issued_at     INTEGER NOT NULL,
            last_seen     INTEGER NOT NULL,
            status        TEXT NOT NULL DEFAULT 'active',
            FOREIGN KEY (license_id) REFERENCES licenses(license_id)
        )
        "#,
    )
    .execute(db)
    .await?;
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_devices_license ON devices(license_id)")
        .execute(db)
        .await?;

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS activation_codes (
            code          TEXT PRIMARY KEY,
            license_id    TEXT NOT NULL,
            created_at    INTEGER NOT NULL,
            expires_at    INTEGER NOT NULL,
            consumed_at   INTEGER,
            FOREIGN KEY (license_id) REFERENCES licenses(license_id)
        )
        "#,
    )
    .execute(db)
    .await?;
    Ok(())
}

// ── Handlers ────────────────────────────────────────────────────────

async fn health() -> impl IntoResponse {
    Json(serde_json::json!({"status": "ok"}))
}

#[derive(Deserialize)]
struct TrialStartRequest {
    email: String,
}

#[derive(Serialize)]
struct MagicLinkResponse {
    magic_link: String,
}

/// `POST /api/trial/start { email }` — provision a 14-day trial license
/// for this email if none exists; otherwise re-issue the activation code
/// for the existing one (idempotent — refreshing the trial doesn't grant
/// a new 14 days).
async fn trial_start(
    State(state): State<AppState>,
    Json(req): Json<TrialStartRequest>,
) -> Result<Json<MagicLinkResponse>, ApiError> {
    let email = req.email.trim().to_lowercase();
    if email.is_empty() || !email.contains('@') {
        return Err(ApiError::bad("email required"));
    }
    let eh = email_hash(&email);

    // Find any non-revoked license for this email. Trial users typically
    // have just one; paid users with an existing license get re-issued
    // an activation code for the EXISTING license rather than a fresh
    // trial — trial-on-top-of-paid would be a UX wart.
    let row: Option<LicenseRow> = sqlx::query_as::<_, LicenseRow>(
        "SELECT license_id, email_hash, tier, issued_at, valid_until, max_devices, status \
         FROM licenses WHERE email_hash = ?1 AND status = 'active' \
         ORDER BY issued_at DESC LIMIT 1",
    )
    .bind(&eh)
    .fetch_optional(&state.db)
    .await?;

    let now = now_secs();
    let license_id = if let Some(row) = row {
        // If it's a *trial* and already expired, the user had their
        // chance — return a clear 409 so the UI can pivot to the paid
        // flow rather than silently re-issuing.
        if row.tier_enum() == Tier::Trial && row.valid_until < now {
            return Err(ApiError::conflict(
                "trial already used and expired — please purchase",
            ));
        }
        row.license_id
    } else {
        // Fresh trial — create the license.
        let lid = ulid_string();
        let issued = now;
        let valid_until = now + token_validity_secs(Tier::Trial);
        sqlx::query(
            "INSERT INTO licenses (license_id, email_hash, tier, issued_at, valid_until, max_devices, status) \
             VALUES (?1, ?2, 'trial', ?3, ?4, 5, 'active')",
        )
        .bind(&lid)
        .bind(&eh)
        .bind(issued)
        .bind(valid_until)
        .execute(&state.db)
        .await?;
        lid
    };

    let code = mint_activation_code(&state.db, &license_id).await?;
    let magic_link = format!("{}/api/activate?code={}", state.public_url, code);

    // PoC "email delivery" — print the link so the operator can copy it.
    // Production replaces this with a Resend call.
    println!("[licensing-server] trial requested for {}", email);
    println!("[licensing-server] magic link: {}", magic_link);

    Ok(Json(MagicLinkResponse { magic_link }))
}

#[derive(Deserialize)]
struct ActivateQuery {
    code: String,
    #[serde(default = "default_device_label")]
    device_label: String,
}

fn default_device_label() -> String {
    "Unknown device".to_string()
}

#[derive(Serialize, Deserialize)]
pub struct TokenResponse {
    pub token: String,
}

/// `GET /api/activate?code=…&device_label=…` — exchange a (one-time-use)
/// activation code for a fresh signed token. Enforces the per-license
/// device limit; if exceeded, returns 429 with the active devices so
/// the UI can show a "deactivate one to continue" picker.
async fn activate(
    State(state): State<AppState>,
    Query(q): Query<ActivateQuery>,
) -> Result<Json<TokenResponse>, ApiError> {
    if q.code.trim().is_empty() {
        return Err(ApiError::bad("code required"));
    }
    if q.device_label.trim().is_empty() {
        return Err(ApiError::bad("device_label required"));
    }

    let now = now_secs();
    // Atomically claim the activation code. If already consumed or
    // expired we reject — codes are single-use.
    let row: Option<ActivationCodeRow> = sqlx::query_as::<_, ActivationCodeRow>(
        "SELECT code, license_id, created_at, expires_at, consumed_at \
         FROM activation_codes WHERE code = ?1",
    )
    .bind(&q.code)
    .fetch_optional(&state.db)
    .await?;
    let Some(row) = row else {
        return Err(ApiError::not_found("unknown activation code"));
    };
    if row.consumed_at.is_some() {
        return Err(ApiError::conflict("activation code already used"));
    }
    if row.expires_at < now {
        return Err(ApiError::conflict("activation code expired"));
    }

    // Look up the license backing this code.
    let lic: Option<LicenseRow> = sqlx::query_as::<_, LicenseRow>(
        "SELECT license_id, email_hash, tier, issued_at, valid_until, max_devices, status \
         FROM licenses WHERE license_id = ?1",
    )
    .bind(&row.license_id)
    .fetch_optional(&state.db)
    .await?;
    let Some(lic) = lic else {
        return Err(ApiError::not_found("license not found"));
    };
    if lic.status != "active" {
        return Err(ApiError::conflict("license suspended"));
    }

    // Device-limit check.
    let active_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM devices WHERE license_id = ?1 AND status = 'active'",
    )
    .bind(&lic.license_id)
    .fetch_one(&state.db)
    .await?;
    if active_count >= lic.max_devices {
        return Err(ApiError::too_many(format!(
            "device limit {} reached — deactivate one to continue",
            lic.max_devices
        )));
    }

    // Insert device + consume code + sign token, all in one transaction
    // so a partial failure can't leave the DB in a state where a code
    // is consumed but no token issued.
    let device_id = ulid_string();
    let mut tx = state.db.begin().await?;
    sqlx::query(
        "UPDATE activation_codes SET consumed_at = ?1 WHERE code = ?2 AND consumed_at IS NULL",
    )
    .bind(now)
    .bind(&q.code)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO devices (device_id, license_id, device_label, issued_at, last_seen, status) \
         VALUES (?1, ?2, ?3, ?4, ?4, 'active')",
    )
    .bind(&device_id)
    .bind(&lic.license_id)
    .bind(&q.device_label)
    .bind(now)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    let claims = Claims {
        v: 1,
        lid: lic.license_id.clone(),
        eh: lic.email_hash.clone(),
        tier: lic.tier_enum(),
        iat: now,
        exp: lic.valid_until,
        max_offline: lic.tier_enum().default_max_offline_days(),
        did: device_id.clone(),
        scope: vec!["cloud".to_string(), "updates".to_string()],
    };
    let token = sign_claims(&state.signing_key, &claims)?;
    Ok(Json(TokenResponse { token }))
}

#[derive(Deserialize)]
struct RefreshRequest {
    token: String,
}

/// `POST /api/refresh { token }` — verify the existing token, bump the
/// device's `last_seen`, and re-issue an updated token (same `lid` and
/// `did`, refreshed `iat`). Used by the client every ~24h to keep the
/// `last_online_check` sidecar fresh.
async fn refresh(
    State(state): State<AppState>,
    Json(req): Json<RefreshRequest>,
) -> Result<Json<TokenResponse>, ApiError> {
    if req.token.trim().is_empty() {
        return Err(ApiError::bad("token required"));
    }
    let claims = verify_token(&state.signing_key.verifying_key(), &req.token)
        .map_err(|e| ApiError::bad(format!("invalid token: {}", e)))?;

    // License + device still active?
    let lic: Option<LicenseRow> = sqlx::query_as::<_, LicenseRow>(
        "SELECT license_id, email_hash, tier, issued_at, valid_until, max_devices, status \
         FROM licenses WHERE license_id = ?1",
    )
    .bind(&claims.lid)
    .fetch_optional(&state.db)
    .await?;
    let Some(lic) = lic else {
        return Err(ApiError::not_found("license not found"));
    };
    if lic.status != "active" {
        return Err(ApiError::conflict("license suspended"));
    }

    let device_status: Option<String> =
        sqlx::query_scalar("SELECT status FROM devices WHERE device_id = ?1 AND license_id = ?2")
            .bind(&claims.did)
            .bind(&lic.license_id)
            .fetch_optional(&state.db)
            .await?;
    match device_status.as_deref() {
        Some("active") => {}
        Some(_) => return Err(ApiError::conflict("device deactivated")),
        None => return Err(ApiError::not_found("device not found")),
    }

    let now = now_secs();
    sqlx::query("UPDATE devices SET last_seen = ?1 WHERE device_id = ?2")
        .bind(now)
        .bind(&claims.did)
        .execute(&state.db)
        .await?;

    // Re-issue with refreshed `iat` but same `exp` (we don't auto-extend
    // — that happens on tier renewal only).
    let new_claims = Claims {
        v: 1,
        lid: lic.license_id.clone(),
        eh: lic.email_hash.clone(),
        tier: lic.tier_enum(),
        iat: now,
        exp: lic.valid_until,
        max_offline: lic.tier_enum().default_max_offline_days(),
        did: claims.did.clone(),
        scope: claims.scope.clone(),
    };
    let token = sign_claims(&state.signing_key, &new_claims)?;
    Ok(Json(TokenResponse { token }))
}

#[derive(Deserialize)]
struct LicenseIssueRequest {
    email: String,
    tier: Tier,
}

/// `POST /api/license/issue { email, tier }` — simulates the Lemon Squeezy
/// purchase webhook. Creates a paid license + activation code, prints
/// the magic link to stdout. In production this is gated by webhook
/// signature verification; in PoC it's open so we can simulate purchases
/// freely from the CLI.
async fn license_issue(
    State(state): State<AppState>,
    Json(req): Json<LicenseIssueRequest>,
) -> Result<Json<MagicLinkResponse>, ApiError> {
    let email = req.email.trim().to_lowercase();
    if email.is_empty() || !email.contains('@') {
        return Err(ApiError::bad("email required"));
    }
    if req.tier == Tier::Trial {
        return Err(ApiError::bad("/issue is for paid tiers; use /trial/start"));
    }
    let eh = email_hash(&email);
    let now = now_secs();
    let lid = ulid_string();
    let valid_until = now + token_validity_secs(req.tier);
    let tier_str = serde_json::to_value(req.tier)
        .ok()
        .and_then(|v| v.as_str().map(|s| s.to_string()))
        .unwrap_or_default();
    sqlx::query(
        "INSERT INTO licenses (license_id, email_hash, tier, issued_at, valid_until, max_devices, status) \
         VALUES (?1, ?2, ?3, ?4, ?5, 5, 'active')",
    )
    .bind(&lid)
    .bind(&eh)
    .bind(&tier_str)
    .bind(now)
    .bind(valid_until)
    .execute(&state.db)
    .await?;
    let code = mint_activation_code(&state.db, &lid).await?;
    let magic_link = format!("{}/api/activate?code={}", state.public_url, code);
    println!(
        "[licensing-server] purchase simulated for {} (tier={:?})",
        email, req.tier
    );
    println!("[licensing-server] magic link: {}", magic_link);
    Ok(Json(MagicLinkResponse { magic_link }))
}

#[derive(Deserialize)]
struct StatusQuery {
    email: Option<String>,
    license_id: Option<String>,
}

/// `GET /api/license/status?email=…` (or `?license_id=…`) — debug endpoint.
/// Lists active licenses + devices for the given identifier. Not for
/// production; lets us inspect state in the PoC.
async fn license_status_debug(
    State(state): State<AppState>,
    Query(q): Query<StatusQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let licenses: Vec<LicenseRow> = if let Some(email) = q.email {
        let eh = email_hash(&email.trim().to_lowercase());
        sqlx::query_as::<_, LicenseRow>(
            "SELECT license_id, email_hash, tier, issued_at, valid_until, max_devices, status \
             FROM licenses WHERE email_hash = ?1",
        )
        .bind(&eh)
        .fetch_all(&state.db)
        .await?
    } else if let Some(lid) = q.license_id {
        sqlx::query_as::<_, LicenseRow>(
            "SELECT license_id, email_hash, tier, issued_at, valid_until, max_devices, status \
             FROM licenses WHERE license_id = ?1",
        )
        .bind(&lid)
        .fetch_all(&state.db)
        .await?
    } else {
        return Err(ApiError::bad("email or license_id required"));
    };

    let mut out = Vec::new();
    for lic in licenses {
        let devices: Vec<DeviceRow> = sqlx::query_as::<_, DeviceRow>(
            "SELECT device_id, license_id, device_label, issued_at, last_seen, status \
             FROM devices WHERE license_id = ?1 ORDER BY issued_at",
        )
        .bind(&lic.license_id)
        .fetch_all(&state.db)
        .await?;
        out.push(serde_json::json!({
            "license_id": lic.license_id,
            "email_hash": lic.email_hash,
            "tier": lic.tier,
            "issued_at": lic.issued_at,
            "valid_until": lic.valid_until,
            "status": lic.status,
            "max_devices": lic.max_devices,
            "devices": devices.into_iter().map(|d| serde_json::json!({
                "device_id": d.device_id,
                "label": d.device_label,
                "issued_at": d.issued_at,
                "last_seen": d.last_seen,
                "status": d.status,
            })).collect::<Vec<_>>(),
        }));
    }
    Ok(Json(serde_json::json!(out)))
}

// ── Helpers ─────────────────────────────────────────────────────────

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn email_hash(email: &str) -> String {
    let mut h = Sha256::new();
    h.update(email.as_bytes());
    let bytes = h.finalize();
    let mut out = String::with_capacity(64);
    for b in bytes {
        out.push_str(&format!("{:02x}", b));
    }
    out
}

/// 26-char Crockford base32 ULID — sortable, URL-safe, 128 bits.
/// Hand-rolled to avoid pulling in a crate just for this.
fn ulid_string() -> String {
    let mut rng = OsRng;
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let mut bytes = [0u8; 16];
    bytes[0] = ((now_ms >> 40) & 0xFF) as u8;
    bytes[1] = ((now_ms >> 32) & 0xFF) as u8;
    bytes[2] = ((now_ms >> 24) & 0xFF) as u8;
    bytes[3] = ((now_ms >> 16) & 0xFF) as u8;
    bytes[4] = ((now_ms >> 8) & 0xFF) as u8;
    bytes[5] = (now_ms & 0xFF) as u8;
    rng.fill(&mut bytes[6..]);
    base32_crockford(&bytes)
}

fn base32_crockford(bytes: &[u8; 16]) -> String {
    const ALPHA: &[u8] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    // 16 bytes = 128 bits → ceil(128/5) = 26 chars.
    let mut out = String::with_capacity(26);
    let mut bits = 0u64;
    let mut n_bits = 0;
    let mut idx = 0;
    while idx < bytes.len() || n_bits >= 5 {
        if n_bits < 5 && idx < bytes.len() {
            bits = (bits << 8) | bytes[idx] as u64;
            n_bits += 8;
            idx += 1;
        }
        n_bits -= 5;
        let v = ((bits >> n_bits) & 0b11111) as usize;
        out.push(ALPHA[v] as char);
    }
    out
}

/// 32-char alphanumeric activation code. Not security-critical (it's
/// short-lived + single-use), but use OsRng anyway for hygiene.
fn activation_code() -> String {
    const CHARS: &[u8] = b"ABCDEFGHJKMNPQRSTUVWXYZabcdefghijkmnpqrstuvwxyz23456789";
    let mut rng = OsRng;
    (0..32)
        .map(|_| CHARS[rng.gen_range(0..CHARS.len())] as char)
        .collect()
}

async fn mint_activation_code(db: &SqlitePool, license_id: &str) -> Result<String, sqlx::Error> {
    let code = activation_code();
    let now = now_secs();
    sqlx::query(
        "INSERT INTO activation_codes (code, license_id, created_at, expires_at, consumed_at) \
         VALUES (?1, ?2, ?3, ?4, NULL)",
    )
    .bind(&code)
    .bind(license_id)
    .bind(now)
    .bind(now + ACTIVATION_CODE_TTL_SECS)
    .execute(db)
    .await?;
    Ok(code)
}

fn b64_url(bytes: &[u8]) -> String {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
    URL_SAFE_NO_PAD.encode(bytes)
}

fn b64_url_decode(s: &str) -> Result<Vec<u8>, String> {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
    URL_SAFE_NO_PAD.decode(s).map_err(|e| e.to_string())
}

/// Sign a Claims struct into a JWT-like Ed25519 token.
fn sign_claims(sk: &SigningKey, claims: &Claims) -> Result<String, ApiError> {
    let header_b64 = b64_url(br#"{"alg":"EdDSA","typ":"DLT"}"#);
    let payload_bytes =
        serde_json::to_vec(claims).map_err(|e| ApiError::internal(e.to_string()))?;
    let payload_b64 = b64_url(&payload_bytes);
    let signing_input = format!("{}.{}", header_b64, payload_b64);
    let sig = sk.sign(signing_input.as_bytes());
    Ok(format!(
        "{}.{}.{}",
        header_b64,
        payload_b64,
        b64_url(&sig.to_bytes())
    ))
}

/// Server-side verify (used by /refresh to validate inbound tokens).
fn verify_token(vk: &VerifyingKey, token: &str) -> Result<Claims, String> {
    use ed25519_dalek::{Signature, Verifier};
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 {
        return Err(format!("expected 3 segments, got {}", parts.len()));
    }
    let sig_bytes = b64_url_decode(parts[2])?;
    let sig_arr: [u8; 64] = sig_bytes
        .as_slice()
        .try_into()
        .map_err(|_| "sig must be 64 bytes".to_string())?;
    let sig = Signature::from_bytes(&sig_arr);
    let signing_input = format!("{}.{}", parts[0], parts[1]);
    vk.verify(signing_input.as_bytes(), &sig)
        .map_err(|e| format!("verify: {}", e))?;
    let payload_bytes = b64_url_decode(parts[1])?;
    let claims: Claims =
        serde_json::from_slice(&payload_bytes).map_err(|e| format!("claims parse: {}", e))?;
    Ok(claims)
}

// ── DB row types ────────────────────────────────────────────────────

#[derive(sqlx::FromRow, Clone)]
struct LicenseRow {
    license_id: String,
    email_hash: String,
    tier: String,
    issued_at: i64,
    valid_until: i64,
    max_devices: i64,
    status: String,
}

impl LicenseRow {
    fn tier_enum(&self) -> Tier {
        match self.tier.as_str() {
            "trial" => Tier::Trial,
            "annual" => Tier::Annual,
            "3year" => Tier::ThreeYear,
            other => panic!("unknown tier in DB: {}", other),
        }
    }
}

#[derive(sqlx::FromRow)]
struct DeviceRow {
    device_id: String,
    #[allow(dead_code)] // kept for future per-device queries
    license_id: String,
    device_label: String,
    issued_at: i64,
    last_seen: i64,
    status: String,
}

#[derive(sqlx::FromRow)]
struct ActivationCodeRow {
    #[allow(dead_code)] // sqlx requires reading the column even if we don't reuse it
    code: String,
    license_id: String,
    #[allow(dead_code)]
    created_at: i64,
    expires_at: i64,
    consumed_at: Option<i64>,
}

// ── Error type ──────────────────────────────────────────────────────

/// API error envelope. Keeps wire shape consistent across endpoints
/// (`{"error": "..."}`) so clients don't have to branch on body shape.
struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    fn bad(msg: impl Into<String>) -> Self {
        ApiError {
            status: StatusCode::BAD_REQUEST,
            message: msg.into(),
        }
    }
    fn not_found(msg: impl Into<String>) -> Self {
        ApiError {
            status: StatusCode::NOT_FOUND,
            message: msg.into(),
        }
    }
    fn conflict(msg: impl Into<String>) -> Self {
        ApiError {
            status: StatusCode::CONFLICT,
            message: msg.into(),
        }
    }
    fn too_many(msg: impl Into<String>) -> Self {
        ApiError {
            status: StatusCode::TOO_MANY_REQUESTS,
            message: msg.into(),
        }
    }
    fn internal(msg: impl Into<String>) -> Self {
        ApiError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: msg.into(),
        }
    }
}

impl From<sqlx::Error> for ApiError {
    fn from(e: sqlx::Error) -> Self {
        ApiError::internal(format!("db: {}", e))
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        (
            self.status,
            Json(serde_json::json!({"error": self.message})),
        )
            .into_response()
    }
}
