-- Dimmy licensing — initial schema.
-- Mirrors the PoC SQLite schema in core/src/license_server.rs::migrate.
-- Field types adapted for D1 (SQLite-compatible — no diff in practice).

CREATE TABLE IF NOT EXISTS licenses (
    license_id    TEXT PRIMARY KEY,
    email_hash    TEXT NOT NULL,
    tier          TEXT NOT NULL,
    issued_at     INTEGER NOT NULL,
    valid_until   INTEGER NOT NULL,
    max_devices   INTEGER NOT NULL DEFAULT 5,
    status        TEXT NOT NULL DEFAULT 'active',
    -- Stripe integration: link the license back to the checkout session
    -- that created it, for refund matching and audit.
    stripe_session_id TEXT,
    stripe_customer_id TEXT
);
CREATE INDEX IF NOT EXISTS idx_licenses_email ON licenses(email_hash);
CREATE INDEX IF NOT EXISTS idx_licenses_stripe ON licenses(stripe_session_id);

CREATE TABLE IF NOT EXISTS devices (
    device_id     TEXT PRIMARY KEY,
    license_id    TEXT NOT NULL,
    device_label  TEXT NOT NULL,
    issued_at     INTEGER NOT NULL,
    last_seen     INTEGER NOT NULL,
    status        TEXT NOT NULL DEFAULT 'active',
    FOREIGN KEY (license_id) REFERENCES licenses(license_id)
);
CREATE INDEX IF NOT EXISTS idx_devices_license ON devices(license_id);

CREATE TABLE IF NOT EXISTS activation_codes (
    code          TEXT PRIMARY KEY,
    license_id    TEXT NOT NULL,
    created_at    INTEGER NOT NULL,
    expires_at    INTEGER NOT NULL,
    consumed_at   INTEGER,
    FOREIGN KEY (license_id) REFERENCES licenses(license_id)
);

-- Idempotency log for Stripe webhook events. Stripe may send the same
-- event_id multiple times (network retries); we accept once, ignore
-- subsequent. Without this table, refund-then-retry would double-revoke.
CREATE TABLE IF NOT EXISTS stripe_events (
    event_id      TEXT PRIMARY KEY,
    received_at   INTEGER NOT NULL,
    type          TEXT NOT NULL
);

-- GDPR / audit log: any operation that touches PII is logged here so
-- we can produce a Subject Access Request (SAR) report or a deletion
-- audit trail on demand. Email_hash is the only identifier; plain
-- email never appears in logs.
CREATE TABLE IF NOT EXISTS audit_log (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    timestamp     INTEGER NOT NULL,
    event_type    TEXT NOT NULL,
    email_hash    TEXT,
    license_id    TEXT,
    details       TEXT  -- free-form JSON, e.g. {"reason": "user requested"}
);
CREATE INDEX IF NOT EXISTS idx_audit_email ON audit_log(email_hash);
