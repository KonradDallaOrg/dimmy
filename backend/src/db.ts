// D1 query helpers — one file per table for clarity.
//
// Mirrors the schema in migrations/0001_initial.sql. Type-safe wrappers
// around `env.DB.prepare(...).bind(...).first/all/run`. Worker code
// imports these helpers and never hand-writes SQL inline — keeps the
// query surface auditable and makes the eventual schema migrations
// trivial to grep for.

export interface LicenseRow {
  license_id: string;
  email_hash: string;
  tier: "trial" | "annual" | "3year";
  issued_at: number;
  valid_until: number;
  max_devices: number;
  status: "active" | "revoked" | "deleted";
  stripe_session_id: string | null;
  stripe_customer_id: string | null;
}

export interface DeviceRow {
  device_id: string;
  license_id: string;
  device_label: string;
  issued_at: number;
  last_seen: number;
  status: "active" | "revoked";
}

export interface ActivationCodeRow {
  code: string;
  license_id: string;
  created_at: number;
  expires_at: number;
  consumed_at: number | null;
}

const COLS_LIC =
  "license_id, email_hash, tier, issued_at, valid_until, max_devices, status, stripe_session_id, stripe_customer_id";

// ── licenses ────────────────────────────────────────────────────────

export async function findActiveLicenseByEmail(
  db: D1Database,
  emailHash: string
): Promise<LicenseRow | null> {
  return db
    .prepare(
      `SELECT ${COLS_LIC} FROM licenses
       WHERE email_hash = ?1 AND status = 'active'
       ORDER BY issued_at DESC LIMIT 1`
    )
    .bind(emailHash)
    .first<LicenseRow>();
}

export async function findLicenseById(
  db: D1Database,
  licenseId: string
): Promise<LicenseRow | null> {
  return db
    .prepare(`SELECT ${COLS_LIC} FROM licenses WHERE license_id = ?1`)
    .bind(licenseId)
    .first<LicenseRow>();
}

export async function findLicenseByStripeSession(
  db: D1Database,
  sessionId: string
): Promise<LicenseRow | null> {
  return db
    .prepare(`SELECT ${COLS_LIC} FROM licenses WHERE stripe_session_id = ?1`)
    .bind(sessionId)
    .first<LicenseRow>();
}

export interface CreateLicenseInput {
  license_id: string;
  email_hash: string;
  tier: "trial" | "annual" | "3year";
  issued_at: number;
  valid_until: number;
  stripe_session_id?: string | null;
  stripe_customer_id?: string | null;
}

export async function insertLicense(
  db: D1Database,
  lic: CreateLicenseInput
): Promise<void> {
  await db
    .prepare(
      `INSERT INTO licenses (
         license_id, email_hash, tier, issued_at, valid_until,
         max_devices, status, stripe_session_id, stripe_customer_id
       ) VALUES (?1, ?2, ?3, ?4, ?5, 5, 'active', ?6, ?7)`
    )
    .bind(
      lic.license_id,
      lic.email_hash,
      lic.tier,
      lic.issued_at,
      lic.valid_until,
      lic.stripe_session_id ?? null,
      lic.stripe_customer_id ?? null
    )
    .run();
}

export async function setLicenseStatus(
  db: D1Database,
  licenseId: string,
  status: "active" | "revoked" | "deleted"
): Promise<void> {
  await db
    .prepare(`UPDATE licenses SET status = ?1 WHERE license_id = ?2`)
    .bind(status, licenseId)
    .run();
}

// ── devices ─────────────────────────────────────────────────────────

export async function countActiveDevices(
  db: D1Database,
  licenseId: string
): Promise<number> {
  const row = await db
    .prepare(
      `SELECT COUNT(*) as n FROM devices
       WHERE license_id = ?1 AND status = 'active'`
    )
    .bind(licenseId)
    .first<{ n: number }>();
  return row?.n ?? 0;
}

export async function listActiveDevices(
  db: D1Database,
  licenseId: string
): Promise<DeviceRow[]> {
  const res = await db
    .prepare(
      `SELECT device_id, license_id, device_label, issued_at, last_seen, status
       FROM devices WHERE license_id = ?1 AND status = 'active'
       ORDER BY issued_at`
    )
    .bind(licenseId)
    .all<DeviceRow>();
  return res.results;
}

export async function insertDevice(
  db: D1Database,
  d: {
    device_id: string;
    license_id: string;
    device_label: string;
    now: number;
  }
): Promise<void> {
  await db
    .prepare(
      `INSERT INTO devices (device_id, license_id, device_label, issued_at, last_seen, status)
       VALUES (?1, ?2, ?3, ?4, ?4, 'active')`
    )
    .bind(d.device_id, d.license_id, d.device_label, d.now)
    .run();
}

export async function bumpDeviceLastSeen(
  db: D1Database,
  deviceId: string,
  now: number
): Promise<void> {
  await db
    .prepare(`UPDATE devices SET last_seen = ?1 WHERE device_id = ?2`)
    .bind(now, deviceId)
    .run();
}

export async function findDeviceStatus(
  db: D1Database,
  deviceId: string,
  licenseId: string
): Promise<string | null> {
  const row = await db
    .prepare(
      `SELECT status FROM devices WHERE device_id = ?1 AND license_id = ?2`
    )
    .bind(deviceId, licenseId)
    .first<{ status: string }>();
  return row?.status ?? null;
}

// ── activation_codes ────────────────────────────────────────────────

export async function insertActivationCode(
  db: D1Database,
  c: {
    code: string;
    license_id: string;
    created_at: number;
    expires_at: number;
  }
): Promise<void> {
  await db
    .prepare(
      `INSERT INTO activation_codes (code, license_id, created_at, expires_at, consumed_at)
       VALUES (?1, ?2, ?3, ?4, NULL)`
    )
    .bind(c.code, c.license_id, c.created_at, c.expires_at)
    .run();
}

export async function findActivationCode(
  db: D1Database,
  code: string
): Promise<ActivationCodeRow | null> {
  return db
    .prepare(
      `SELECT code, license_id, created_at, expires_at, consumed_at
       FROM activation_codes WHERE code = ?1`
    )
    .bind(code)
    .first<ActivationCodeRow>();
}

export async function consumeActivationCode(
  db: D1Database,
  code: string,
  now: number
): Promise<boolean> {
  // UPDATE … WHERE consumed_at IS NULL is the atomic claim. If a parallel
  // activation already consumed the code, our UPDATE matches 0 rows and
  // we return false → caller errors with "already used".
  const r = await db
    .prepare(
      `UPDATE activation_codes SET consumed_at = ?1
       WHERE code = ?2 AND consumed_at IS NULL`
    )
    .bind(now, code)
    .run();
  return (r.meta.changes ?? 0) > 0;
}

// ── stripe_events (idempotency) ─────────────────────────────────────

export async function recordStripeEvent(
  db: D1Database,
  eventId: string,
  type: string,
  now: number
): Promise<boolean> {
  // INSERT OR IGNORE returns 0 changes if the event was already processed.
  const r = await db
    .prepare(
      `INSERT OR IGNORE INTO stripe_events (event_id, received_at, type)
       VALUES (?1, ?2, ?3)`
    )
    .bind(eventId, now, type)
    .run();
  return (r.meta.changes ?? 0) > 0;
}

// ── audit_log ───────────────────────────────────────────────────────

export async function audit(
  db: D1Database,
  ev: {
    event_type: string;
    email_hash?: string | null;
    license_id?: string | null;
    details?: Record<string, unknown>;
  },
  now: number
): Promise<void> {
  await db
    .prepare(
      `INSERT INTO audit_log (timestamp, event_type, email_hash, license_id, details)
       VALUES (?1, ?2, ?3, ?4, ?5)`
    )
    .bind(
      now,
      ev.event_type,
      ev.email_hash ?? null,
      ev.license_id ?? null,
      ev.details ? JSON.stringify(ev.details) : null
    )
    .run();
}
