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
  tier: "trial" | "monthly" | "annual" | "lifetime";
  issued_at: number;
  valid_until: number;
  max_devices: number;
  status: "active" | "past_due" | "revoked" | "deleted";
  stripe_session_id: string | null;
  stripe_customer_id: string | null;
  stripe_subscription_id: string | null;
  current_period_end: number | null;
  cancel_at_period_end: number;
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
  "license_id, email_hash, tier, issued_at, valid_until, max_devices, status, stripe_session_id, stripe_customer_id, stripe_subscription_id, current_period_end, cancel_at_period_end";

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
  tier: "trial" | "monthly" | "annual" | "lifetime";
  issued_at: number;
  valid_until: number;
  stripe_session_id?: string | null;
  stripe_customer_id?: string | null;
  stripe_subscription_id?: string | null;
  current_period_end?: number | null;
}

export async function insertLicense(
  db: D1Database,
  lic: CreateLicenseInput
): Promise<void> {
  await db
    .prepare(
      `INSERT INTO licenses (
         license_id, email_hash, tier, issued_at, valid_until,
         max_devices, status, stripe_session_id, stripe_customer_id,
         stripe_subscription_id, current_period_end, cancel_at_period_end
       ) VALUES (?1, ?2, ?3, ?4, ?5, 5, 'active', ?6, ?7, ?8, ?9, 0)`
    )
    .bind(
      lic.license_id,
      lic.email_hash,
      lic.tier,
      lic.issued_at,
      lic.valid_until,
      lic.stripe_session_id ?? null,
      lic.stripe_customer_id ?? null,
      lic.stripe_subscription_id ?? null,
      lic.current_period_end ?? null
    )
    .run();
}

/// In-place "lifetime wins" upgrade: bumps an existing monthly/annual
/// license to lifetime tier, extends `valid_until` to the new lifetime
/// horizon, drops the recurring-subscription bookkeeping (which the
/// caller has just cancelled via the Stripe API), and re-pins
/// `stripe_session_id` to the lifetime checkout that triggered the
/// upgrade so subsequent webhook retries find this row by session id
/// and idempotency-skip.
///
/// `stripe_customer_id` is updated only when the new value is non-null
/// (Stripe usually reuses the same Customer for the same email — keep
/// the existing one if Stripe didn't echo a new one).
export async function upgradeLicenseToLifetime(
  db: D1Database,
  licenseId: string,
  patch: {
    new_valid_until: number;
    new_session_id: string;
    new_customer_id: string | null;
  }
): Promise<number> {
  const r = await db
    .prepare(
      `UPDATE licenses SET
         tier                  = 'lifetime',
         valid_until           = ?1,
         current_period_end    = NULL,
         cancel_at_period_end  = 0,
         stripe_subscription_id= NULL,
         stripe_session_id     = ?2,
         stripe_customer_id    = COALESCE(?3, stripe_customer_id)
       WHERE license_id = ?4`
    )
    .bind(
      patch.new_valid_until,
      patch.new_session_id,
      patch.new_customer_id ?? null,
      licenseId
    )
    .run();
  return r.meta.changes ?? 0;
}

export async function setLicenseStatus(
  db: D1Database,
  licenseId: string,
  status: "active" | "past_due" | "revoked" | "deleted"
): Promise<void> {
  await db
    .prepare(`UPDATE licenses SET status = ?1 WHERE license_id = ?2`)
    .bind(status, licenseId)
    .run();
}

/// Look up a license by Stripe subscription ID. Used by every
/// `customer.subscription.*` and `invoice.*` webhook handler to find
/// the row to mutate.
export async function findLicenseBySubscription(
  db: D1Database,
  subscriptionId: string
): Promise<LicenseRow | null> {
  return db
    .prepare(
      `SELECT ${COLS_LIC} FROM licenses WHERE stripe_subscription_id = ?1`
    )
    .bind(subscriptionId)
    .first<LicenseRow>();
}

/// Look up the most recently issued license tied to a Stripe customer.
/// Used by `charge.refunded` (the charge object carries `customer` but
/// not `checkout_session_id` directly). Returns the latest active row;
/// historical rows for the same customer (renewals, re-purchases) are
/// preserved but ignored here.
export async function findActiveLicenseByStripeCustomer(
  db: D1Database,
  customerId: string
): Promise<LicenseRow | null> {
  return db
    .prepare(
      `SELECT ${COLS_LIC} FROM licenses
       WHERE stripe_customer_id = ?1 AND status = 'active'
       ORDER BY issued_at DESC LIMIT 1`
    )
    .bind(customerId)
    .first<LicenseRow>();
}

/// Apply a subscription state change in one atomic UPDATE.
/// Used by `customer.subscription.updated` (period end + cancel flag)
/// and by `invoice.paid` (extends validity to the new period end).
///
/// Pass `null` for fields you don't want to touch — `COALESCE` keeps
/// the existing value.
export async function updateLicenseFromSubscription(
  db: D1Database,
  subscriptionId: string,
  patch: {
    valid_until?: number | null;
    current_period_end?: number | null;
    cancel_at_period_end?: number | null;
    status?: "active" | "past_due" | "revoked" | null;
  }
): Promise<number> {
  const r = await db
    .prepare(
      `UPDATE licenses SET
         valid_until         = COALESCE(?1, valid_until),
         current_period_end  = COALESCE(?2, current_period_end),
         cancel_at_period_end= COALESCE(?3, cancel_at_period_end),
         status              = COALESCE(?4, status)
       WHERE stripe_subscription_id = ?5`
    )
    .bind(
      patch.valid_until ?? null,
      patch.current_period_end ?? null,
      patch.cancel_at_period_end ?? null,
      patch.status ?? null,
      subscriptionId
    )
    .run();
  return r.meta.changes ?? 0;
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

/// All devices for a license, regardless of status. UI Settings → License
/// shows them all so the user can see their device history (and reactivate
/// a previously-revoked one if they want — by reusing the same machine).
export async function listAllDevices(
  db: D1Database,
  licenseId: string
): Promise<DeviceRow[]> {
  const res = await db
    .prepare(
      `SELECT device_id, license_id, device_label, issued_at, last_seen, status
       FROM devices WHERE license_id = ?1
       ORDER BY issued_at`
    )
    .bind(licenseId)
    .all<DeviceRow>();
  return res.results;
}

/// Mark a device revoked. Idempotent — if already revoked, no-op.
/// Returns the number of rows affected (0 = already revoked or unknown).
export async function deactivateDeviceById(
  db: D1Database,
  deviceId: string,
  licenseId: string
): Promise<number> {
  const res = await db
    .prepare(
      `UPDATE devices SET status = 'revoked'
       WHERE device_id = ?1 AND license_id = ?2 AND status = 'active'`
    )
    .bind(deviceId, licenseId)
    .run();
  return res.meta.changes ?? 0;
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

/// Most recent un-consumed activation code for a license, minted within
/// the idempotency window. Used by trial/start and by the
/// duplicate-purchase magic-link flow to dedup magic-link emails: if
/// the user double-clicks "Start trial" or the page reloads twice
/// within 5 minutes, we re-send the SAME code instead of minting a new
/// one. Two emails arrive (Resend has its own dedup window we don't
/// rely on) but BOTH carry the same code — clicking either works
/// idempotently.
///
/// `since_secs` is the floor on `created_at` — pass `now - 300` for a
/// 5-minute window.
export async function findRecentUnconsumedActivationCode(
  db: D1Database,
  licenseId: string,
  since_secs: number
): Promise<ActivationCodeRow | null> {
  return db
    .prepare(
      `SELECT code, license_id, created_at, expires_at, consumed_at
       FROM activation_codes
       WHERE license_id = ?1
         AND consumed_at IS NULL
         AND created_at > ?2
       ORDER BY created_at DESC LIMIT 1`
    )
    .bind(licenseId, since_secs)
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
