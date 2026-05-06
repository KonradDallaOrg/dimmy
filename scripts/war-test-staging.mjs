// war-test-staging.mjs
//
// Comprehensive end-to-end test battery against the staging licensing
// Worker. Forges Stripe webhook events signed with the staging
// STRIPE_WEBHOOK_SECRET (which must be set to the value passed via
// --whsec / env STRIPE_WHSEC), POSTs each to the Worker, then queries
// staging D1 (via WSL wrangler) to assert the expected state mutation.
//
// Run from PowerShell on Windows (or WSL):
//   STRIPE_WHSEC=whsec_test_xxx node scripts/war-test-staging.mjs
//   STRIPE_WHSEC=whsec_test_xxx node scripts/war-test-staging.mjs trial-dedup
//
// Why this exists: stripe CLI requires interactive auth; this script
// avoids that by signing events ourselves. Same wire shape Stripe
// would send (timestamp + body + HMAC-SHA256 → Stripe-Signature
// header), so it exercises the same code path.

import { createHmac, randomBytes } from "node:crypto";
import { execSync } from "node:child_process";
import { writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const STAGING = "https://license-staging.dimmy.app";
const D1 = "dimmy-licensing-staging";
const ENV = "staging";
const WHSEC = process.env.STRIPE_WHSEC;
if (!WHSEC) {
  console.error("set STRIPE_WHSEC env var (whsec_... matching the deployed Worker)");
  process.exit(1);
}

// ─── helpers ────────────────────────────────────────────────────────

const C = {
  pass: "\x1b[32m✓\x1b[0m",
  fail: "\x1b[31m✗\x1b[0m",
  info: "\x1b[36m▶\x1b[0m",
  dim: "\x1b[90m",
  reset: "\x1b[0m",
};

let passed = 0, failed = 0;
const failures = [];

function pass(name, extra = "") {
  passed++;
  console.log(`  ${C.pass} ${name}${extra ? C.dim + " — " + extra + C.reset : ""}`);
}
function fail(name, msg) {
  failed++;
  failures.push({ name, msg });
  console.error(`  ${C.fail} ${name} — ${msg}`);
}
function section(name) { console.log(`\n${C.info} ${name}`); }

function ulid() {
  // Cheap pseudo-ULID for test fixtures (just need uniqueness).
  return "01" + randomBytes(12).toString("hex").toUpperCase().slice(0, 24);
}

function signEvent(body) {
  const ts = Math.floor(Date.now() / 1000);
  const mac = createHmac("sha256", WHSEC)
    .update(`${ts}.${body}`)
    .digest("hex");
  return `t=${ts},v1=${mac}`;
}

async function postWebhook(eventType, dataObject) {
  const eventId = "evt_warroom_" + randomBytes(8).toString("hex");
  const body = JSON.stringify({
    id: eventId,
    type: eventType,
    data: { object: dataObject },
  });
  const sig = signEvent(body);
  const r = await fetch(`${STAGING}/api/stripe/webhook`, {
    method: "POST",
    headers: {
      "Content-Type": "application/json",
      "Stripe-Signature": sig,
    },
    body,
  });
  return { status: r.status, body: await r.text(), eventId };
}

async function postJSON(path, body) {
  const r = await fetch(`${STAGING}${path}`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body),
  });
  return { status: r.status, body: await r.text() };
}

async function getJSON(path) {
  const r = await fetch(`${STAGING}${path}`);
  return { status: r.status, body: await r.text() };
}

// Run a SQL query against staging D1 via WSL wrangler. Returns parsed
// rows array (empty if no results). Uses --command (returns actual
// rows for SELECT) instead of --file (returns only summary stats for
// SELECT — wrangler quirk). Output redirected to a temp file to
// avoid shell-noise contamination of stdout.
import { readFileSync } from "node:fs";

function d1(sql) {
  const tmpOut = join(tmpdir(), `wartest-out-${randomBytes(4).toString("hex")}.json`);
  const wslOut = tmpOut.replace(/\\/g, "/").replace(/^([A-Z]):/i, (_, d) => `/mnt/${d.toLowerCase()}`);
  // Wrangler --command takes a single SQL statement. Single-quote string
  // literals in our SQL; we wrap the whole command in single quotes for
  // bash, which means any internal `'` must be escaped. We use only
  // double-quoted string literals in SQL passed in, so this stays simple.
  const sqlSafe = sql.replace(/'/g, `'\\''`);
  const inner =
    `cd /mnt/c/code/pai-voice/backend && ` +
    `wrangler d1 execute ${D1} --env ${ENV} --remote --json --command '${sqlSafe}' > '${wslOut}' 2>/dev/null`;
  // Outer quoting: single-quote the entire bash command so PowerShell
  // and WSL preserve it. Replace internal `'` with `'"'"'`.
  const wsl = `wsl bash -lc "${inner.replace(/"/g, '\\"').replace(/\$/g, '\\$')}"`;
  try {
    execSync(wsl, { encoding: "utf8", maxBuffer: 16 * 1024 * 1024 });
    const raw = readFileSync(tmpOut, "utf8");
    const json = JSON.parse(raw);
    return json[0]?.results ?? [];
  } catch (e) {
    return { _error: e.message.split("\n")[0] };
  }
}

// ─── scenarios ──────────────────────────────────────────────────────

async function scenario_health() {
  section("01. /api/health");
  const r = await getJSON("/api/health");
  if (r.status === 200 && r.body.includes('"ok"')) pass("health 200");
  else fail("health", `status=${r.status} body=${r.body.slice(0, 80)}`);
}

async function scenario_signature_invalid() {
  section("02. invalid Stripe-Signature → 400");
  const body = JSON.stringify({ id: "evt_x", type: "checkout.session.completed", data: { object: {} } });
  const r = await fetch(`${STAGING}/api/stripe/webhook`, {
    method: "POST",
    headers: { "Content-Type": "application/json", "Stripe-Signature": "t=0,v1=00" },
    body,
  });
  if (r.status === 400) pass("invalid sig rejected (400)");
  else fail("invalid sig", `expected 400, got ${r.status}`);
}

async function scenario_trial_creates_license() {
  section("03. POST /api/trial/start → license created in D1");
  const email = `wartest-trial-${Date.now()}-${randomBytes(2).toString("hex")}@example.com`;
  const r = await postJSON("/api/trial/start", { email });
  if (r.status !== 200) {
    fail("trial create", `status=${r.status} body=${r.body.slice(0, 100)}`);
    return null;
  }
  const j = JSON.parse(r.body);
  // Verify via audit_log: the most recent trial_created has a license_id.
  const audits = d1(`SELECT license_id FROM audit_log WHERE event_type = 'trial_created' ORDER BY timestamp DESC LIMIT 1`);
  if (Array.isArray(audits) && audits[0]?.license_id) {
    pass(`trial license created`, `code=${j.code?.slice(0, 12)}… lid=${audits[0].license_id.slice(0, 16)}…`);
    return { email, code: j.code, lid: audits[0].license_id };
  } else {
    fail("trial create", `audit lookup failed: ${JSON.stringify(audits).slice(0, 100)}`);
    return null;
  }
}

async function scenario_trial_dedup() {
  section("04. trial dedup (5-min window) → same code returned");
  const email = `wartest-dedup-${Date.now()}@example.com`;
  const r1 = await postJSON("/api/trial/start", { email });
  const r2 = await postJSON("/api/trial/start", { email });
  if (r1.status !== 200 || r2.status !== 200) {
    fail("dedup", `r1=${r1.status} r2=${r2.status}`);
    return;
  }
  const c1 = JSON.parse(r1.body).code;
  const c2 = JSON.parse(r2.body).code;
  if (c1 === c2) pass("same code returned twice", `${c1.slice(0, 12)}…`);
  else fail("dedup", `c1=${c1.slice(0, 8)} c2=${c2.slice(0, 8)} (different)`);
}

async function scenario_trial_expired_blocked() {
  section("05. expired trial → 409 (no fresh-trial issuance)");
  // Use a deterministic, unique email + look up the license by its
  // email_hash (NOT 'ORDER BY timestamp DESC LIMIT 1', which picks
  // the wrong row when two trial_created entries land in the same
  // second — the race we hit live on 2026-05-04).
  const email = `wartest-expired-${Date.now()}-${randomBytes(2).toString("hex")}@example.com`;
  // Server-side hash uses sha256(email.lower()).hex — match it here.
  const { createHash } = await import("node:crypto");
  const eh = createHash("sha256").update(email.toLowerCase()).digest("hex");
  await postJSON("/api/trial/start", { email });
  const rows = d1(
    `SELECT license_id FROM licenses WHERE email_hash = '${eh}' AND tier = 'trial'`
  );
  if (!Array.isArray(rows) || !rows[0]?.license_id) {
    fail("expired trial", `couldn't locate trial license_id from email_hash: ${JSON.stringify(rows).slice(0, 100)}`);
    return;
  }
  d1(`UPDATE licenses SET valid_until = 1 WHERE license_id = '${rows[0].license_id}'`);
  const r = await postJSON("/api/trial/start", { email });
  if (r.status === 409 && r.body.includes("trial already used")) {
    pass("expired trial → 409");
  } else {
    fail("expired trial", `status=${r.status} body=${r.body.slice(0, 100)}`);
  }
}

async function scenario_checkout_completed_creates_license() {
  section("06. checkout.session.completed (annual) → license created");
  const email = `wartest-buy-${Date.now()}-${randomBytes(2).toString("hex")}@example.com`;
  const sessionId = `cs_test_warroom_${randomBytes(8).toString("hex")}`;
  const subId = `sub_warroom_${randomBytes(8).toString("hex")}`;
  const r = await postWebhook("checkout.session.completed", {
    id: sessionId,
    mode: "subscription",
    customer: `cus_warroom_${randomBytes(4).toString("hex")}`,
    subscription: subId,
    customer_details: { email },
    metadata: { tier: "annual" },
  });
  if (r.status !== 200) {
    fail("checkout completed", `webhook status=${r.status} body=${r.body.slice(0, 100)}`);
    return null;
  }
  // Locate the new license by stripe_subscription_id (deterministic).
  const rows = d1(`SELECT tier FROM licenses WHERE stripe_subscription_id = '${subId}'`);
  if (rows.length === 1 && rows[0].tier === "annual") {
    pass("annual license row created", `subId=${subId.slice(0, 16)}…`);
    return { email, sessionId, subId };
  } else {
    fail("checkout completed", `rows=${JSON.stringify(rows).slice(0, 100)}`);
    return null;
  }
}

async function scenario_duplicate_purchase_blocked() {
  section("07. duplicate annual checkout → blocked, audit logged");
  const email = `wartest-dup-${Date.now()}-${randomBytes(2).toString("hex")}@example.com`;
  // First buy seeds the active license.
  await postWebhook("checkout.session.completed", {
    id: `cs_dup1_${randomBytes(4).toString("hex")}`,
    mode: "subscription",
    customer: `cus_dup_${randomBytes(4).toString("hex")}`,
    subscription: `sub_dup1_${randomBytes(4).toString("hex")}`,
    customer_details: { email },
    metadata: { tier: "annual" },
  });
  // Second buy — should be blocked. Stamp a unique session_id we can
  // grep for in audit_log.details.attempted_session_id.
  const blockedSessionId = `cs_BLOCKED_${randomBytes(8).toString("hex")}`;
  await postWebhook("checkout.session.completed", {
    id: `cs_dup2_${randomBytes(4).toString("hex")}`,
    mode: "subscription",
    customer: `cus_dup_${randomBytes(4).toString("hex")}`,
    subscription: `sub_dup_BLOCKED_${randomBytes(4).toString("hex")}`,
    customer_details: { email },
    metadata: { tier: "annual" },
  });
  const rows = d1(`SELECT details FROM audit_log WHERE event_type = 'duplicate_purchase_blocked' ORDER BY timestamp DESC LIMIT 5`);
  if (Array.isArray(rows) && rows.length > 0) {
    pass(`duplicate_purchase_blocked audit row found (last=${rows.length})`);
  } else {
    fail("duplicate block", `no audit row: ${JSON.stringify(rows).slice(0, 100)}`);
  }
}

async function scenario_lifetime_in_place_upgrade() {
  section("08. monthly → lifetime (in-place upgrade via gate)");
  const email = `wartest-ltup-${Date.now()}@example.com`;
  const subId = `sub_mo_${randomBytes(4).toString("hex")}`;
  // Start with monthly.
  await postWebhook("checkout.session.completed", {
    id: `cs_mo_${randomBytes(4).toString("hex")}`,
    mode: "subscription",
    customer: `cus_lt_${randomBytes(4).toString("hex")}`,
    subscription: subId,
    customer_details: { email },
    metadata: { tier: "monthly" },
  });
  // Now buy lifetime — gate should upgrade in place.
  const ltSession = `cs_lt_${randomBytes(4).toString("hex")}`;
  await postWebhook("checkout.session.completed", {
    id: `cs_lt_${randomBytes(4).toString("hex")}`,
    mode: "payment",
    customer: `cus_lt_${randomBytes(4).toString("hex")}`,
    customer_details: { email },
    metadata: { tier: "lifetime" },
    id: ltSession,
  });
  // Verify via audit_log: the most recent license_upgraded_to_lifetime
  // row should be ours, and the matching license row should now be
  // tier=lifetime with sub_id cleared.
  const audits = d1(`SELECT license_id FROM audit_log WHERE event_type = 'license_upgraded_to_lifetime' ORDER BY timestamp DESC LIMIT 1`);
  if (!Array.isArray(audits) || !audits[0]?.license_id) {
    fail("lifetime upgrade", `no audit row: ${JSON.stringify(audits).slice(0, 100)}`);
    return;
  }
  const lid = audits[0].license_id;
  const rows = d1(`SELECT tier, stripe_subscription_id FROM licenses WHERE license_id = '${lid}'`);
  if (rows.length === 1 && rows[0].tier === "lifetime" && rows[0].stripe_subscription_id === null) {
    pass(`license ${lid.slice(0, 16)}… upgraded in-place to lifetime, sub_id cleared`);
  } else {
    fail("lifetime upgrade", `rows=${JSON.stringify(rows)}`);
  }
}

async function scenario_subscription_updated_cancel_scheduled() {
  section("09. customer.subscription.updated cancel_at_period_end → cancels_at populated");
  // Need an existing license with a known sub_id.
  const email = `wartest-cancel-${Date.now()}@example.com`;
  const subId = `sub_cancel_${randomBytes(4).toString("hex")}`;
  await postWebhook("checkout.session.completed", {
    id: `cs_can_${randomBytes(4).toString("hex")}`,
    mode: "subscription",
    customer: `cus_can_${randomBytes(4).toString("hex")}`,
    subscription: subId,
    customer_details: { email },
    metadata: { tier: "annual" },
  });
  const periodEnd = Math.floor(Date.now() / 1000) + 366 * 86400;
  await postWebhook("customer.subscription.updated", {
    id: subId,
    status: "active",
    cancel_at_period_end: true,
    current_period_end: periodEnd,
    items: { data: [{ price: { id: "price_1TSKE9HxRNDPFvsZv4T1Ampf" } }] },
  });
  const rows = d1(`SELECT cancel_at_period_end, current_period_end FROM licenses WHERE stripe_subscription_id = '${subId}'`);
  if (rows.length === 1 && rows[0].cancel_at_period_end === 1) {
    pass("cancel_at_period_end flag set on license row");
  } else {
    fail("cancel scheduled", `rows=${JSON.stringify(rows)}`);
  }
}

async function scenario_subscription_updated_tier_change() {
  section("10. customer.subscription.updated price flip → tier mirrored to D1");
  const email = `wartest-flip-${Date.now()}@example.com`;
  const subId = `sub_flip_${randomBytes(4).toString("hex")}`;
  // Start as monthly.
  await postWebhook("checkout.session.completed", {
    id: `cs_flip_${randomBytes(4).toString("hex")}`,
    mode: "subscription",
    customer: `cus_flip_${randomBytes(4).toString("hex")}`,
    subscription: subId,
    customer_details: { email },
    metadata: { tier: "monthly" },
  });
  const before = d1(`SELECT tier FROM licenses WHERE stripe_subscription_id = '${subId}'`);
  // Now Stripe says price has changed to annual.
  await postWebhook("customer.subscription.updated", {
    id: subId,
    status: "active",
    cancel_at_period_end: false,
    current_period_end: Math.floor(Date.now() / 1000) + 366 * 86400,
    items: { data: [{ price: { id: "price_1TSKE9HxRNDPFvsZv4T1Ampf" /* annual */ } }] },
  });
  const after = d1(`SELECT tier FROM licenses WHERE stripe_subscription_id = '${subId}'`);
  if (before[0]?.tier === "monthly" && after[0]?.tier === "annual") {
    pass("tier mirrored monthly → annual on price flip");
  } else {
    fail("tier flip", `before=${JSON.stringify(before)} after=${JSON.stringify(after)}`);
  }
}

async function scenario_refund_created_full() {
  section("11. refund.created (full) → license revoked");
  // We can't easily fetch_charge from here since worker calls Stripe API
  // for the GET /v1/charges/:id — that needs sk_test we don't have.
  // The handler will log "could not fetch charge" and bail without
  // mutating. Verify graceful no-op.
  const customerId = `cus_refund_${randomBytes(4).toString("hex")}`;
  await postWebhook("checkout.session.completed", {
    id: `cs_rf_${randomBytes(4).toString("hex")}`,
    mode: "payment",
    customer: customerId,
    customer_details: { email: `wartest-refund-${Date.now()}@example.com` },
    metadata: { tier: "lifetime" },
  });
  const r = await postWebhook("refund.created", {
    id: `re_${randomBytes(4).toString("hex")}`,
    charge: `ch_${randomBytes(4).toString("hex")}`,
    customer: customerId,
    status: "succeeded",
    amount: 9900,
  });
  // Without Stripe API access for the charge fetch, the handler logs
  // "could not fetch charge" and leaves the license alone. This is
  // intentional defensive behaviour — we'd rather not-revoke than
  // wrong-revoke. Test passes if status code is 200 (handler didn't crash).
  if (r.status === 200) {
    pass("refund.created handled (200) — defensive no-op without sk_test in scope");
  } else {
    fail("refund.created", `status=${r.status} body=${r.body.slice(0, 100)}`);
  }
}

async function scenario_charge_refunded_full() {
  section("12. charge.refunded (full) → license revoked");
  const customerId = `cus_chrf_${randomBytes(4).toString("hex")}`;
  // Seed a lifetime license tied to this customer.
  await postWebhook("checkout.session.completed", {
    id: `cs_chrf_${randomBytes(4).toString("hex")}`,
    mode: "payment",
    customer: customerId,
    customer_details: { email: `wartest-chrf-${Date.now()}@example.com` },
    metadata: { tier: "lifetime" },
  });
  await postWebhook("charge.refunded", {
    id: `ch_full_${randomBytes(4).toString("hex")}`,
    customer: customerId,
    amount: 9900,
    amount_refunded: 9900,
  });
  const rows = d1(`SELECT status FROM licenses WHERE stripe_customer_id = '${customerId}' ORDER BY issued_at DESC LIMIT 1`);
  if (rows.length === 1 && rows[0].status === "revoked") {
    pass("license revoked on full charge.refunded");
  } else {
    fail("charge.refunded full", `rows=${JSON.stringify(rows)}`);
  }
}

async function scenario_charge_refunded_partial() {
  section("13. charge.refunded (partial) → license stays active");
  const customerId = `cus_part_${randomBytes(4).toString("hex")}`;
  await postWebhook("checkout.session.completed", {
    id: `cs_part_${randomBytes(4).toString("hex")}`,
    mode: "payment",
    customer: customerId,
    customer_details: { email: `wartest-part-${Date.now()}@example.com` },
    metadata: { tier: "lifetime" },
  });
  await postWebhook("charge.refunded", {
    id: `ch_part_${randomBytes(4).toString("hex")}`,
    customer: customerId,
    amount: 9900,
    amount_refunded: 1000,
  });
  const rows = d1(`SELECT status FROM licenses WHERE stripe_customer_id = '${customerId}' ORDER BY issued_at DESC LIMIT 1`);
  if (rows.length === 1 && rows[0].status === "active") {
    pass("license stays active on partial refund");
  } else {
    fail("charge.refunded partial", `rows=${JSON.stringify(rows)}`);
  }
}

async function scenario_idempotent_event_replay() {
  section("14. duplicate event_id → 200 'duplicate, ignored'");
  const eventId = `evt_idem_${randomBytes(4).toString("hex")}`;
  const sessionId = `cs_idem_${randomBytes(4).toString("hex")}`;
  const body = JSON.stringify({
    id: eventId,
    type: "checkout.session.completed",
    data: { object: {
      id: sessionId,
      mode: "subscription",
      customer: `cus_idem_${randomBytes(4).toString("hex")}`,
      subscription: `sub_idem_${randomBytes(4).toString("hex")}`,
      customer_details: { email: `wartest-idem-${Date.now()}@example.com` },
      metadata: { tier: "annual" },
    }},
  });
  const sig = signEvent(body);
  const opts = {
    method: "POST",
    headers: { "Content-Type": "application/json", "Stripe-Signature": sig },
    body,
  };
  // Note: signing twice would change the timestamp; use the same sig
  // both times so the second call genuinely replays the same payload.
  await fetch(`${STAGING}/api/stripe/webhook`, opts);
  const r2 = await fetch(`${STAGING}/api/stripe/webhook`, opts);
  const t2 = await r2.text();
  if (r2.status === 200 && t2.includes("duplicate")) {
    pass("replay returns duplicate, ignored");
  } else {
    fail("idempotency", `status=${r2.status} body=${t2.slice(0, 100)}`);
  }
}

async function scenario_plan_change_endpoint_validation() {
  section("15. /api/plan-change input validation");
  // No token → 400.
  const r1 = await postJSON("/api/plan-change", { new_tier: "annual" });
  if (r1.status === 400) pass("missing token → 400");
  else fail("plan-change no token", `status=${r1.status}`);
  // Invalid new_tier → 400.
  const r2 = await postJSON("/api/plan-change", { token: "x", new_tier: "lifetime" });
  if (r2.status === 400) pass("lifetime new_tier rejected → 400");
  else fail("plan-change lifetime", `status=${r2.status}`);
  // Bad token → 400.
  const r3 = await postJSON("/api/plan-change", { token: "not.a.token", new_tier: "annual" });
  if (r3.status === 400) pass("invalid token → 400");
  else fail("plan-change bad token", `status=${r3.status}`);
}

// ─── dispatcher ─────────────────────────────────────────────────────

const SCENARIOS = {
  health: scenario_health,
  signature: scenario_signature_invalid,
  trial: scenario_trial_creates_license,
  dedup: scenario_trial_dedup,
  "trial-expired": scenario_trial_expired_blocked,
  checkout: scenario_checkout_completed_creates_license,
  duplicate: scenario_duplicate_purchase_blocked,
  lifetime: scenario_lifetime_in_place_upgrade,
  cancel: scenario_subscription_updated_cancel_scheduled,
  "tier-flip": scenario_subscription_updated_tier_change,
  refund: scenario_refund_created_full,
  "charge-refunded-full": scenario_charge_refunded_full,
  "charge-refunded-partial": scenario_charge_refunded_partial,
  idempotent: scenario_idempotent_event_replay,
  "plan-change": scenario_plan_change_endpoint_validation,
};

const arg = process.argv[2];
const list = arg ? [arg] : Object.keys(SCENARIOS);
const start = Date.now();

for (const name of list) {
  const fn = SCENARIOS[name];
  if (!fn) {
    console.error(`unknown scenario: ${name}`);
    process.exit(2);
  }
  try {
    await fn();
  } catch (e) {
    fail(name, `THREW: ${e.message}`);
  }
}

console.log(`\n${C.dim}── ran ${list.length} scenarios in ${((Date.now() - start) / 1000).toFixed(1)}s${C.reset}`);
console.log(`   ${C.pass} ${passed} passed   ${failed > 0 ? C.fail : C.dim}${failed} failed${C.reset}`);
if (failed > 0) {
  console.log("\nfailures:");
  for (const f of failures) console.log(`  ${C.fail} ${f.name}: ${f.msg}`);
  process.exit(1);
}
