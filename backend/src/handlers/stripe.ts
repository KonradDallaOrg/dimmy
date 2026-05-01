// POST /api/stripe/webhook — Stripe webhook handler.
//
// Validates the Stripe-Signature header (HMAC-SHA256 with the webhook
// signing secret) before trusting the body. Handles:
//   - checkout.session.completed → create license, send activation email
//   - charge.refunded             → revoke license
//   - customer.subscription.deleted → revoke license (future, when we
//                                     add subscriptions; one-shot today)
//
// Idempotent via the stripe_events table: we INSERT OR IGNORE the
// event_id; if a duplicate webhook arrives (Stripe retries on 5xx),
// we no-op. Without this, refund-then-retry would double-revoke and
// charge.refunded for an already-refunded customer would log spurious
// errors.

import type { Env } from "../index";
import { json } from "../index";
import {
  audit,
  findLicenseByStripeSession,
  insertActivationCode,
  insertLicense,
  recordStripeEvent,
  setLicenseStatus,
} from "../db";
import { activationCode, emailHash, ulid } from "../crypto";
import { sendActivationEmail } from "../email";

const ANNUAL_VALIDITY_SECS = 365 * 86_400;
const THREE_YEAR_VALIDITY_SECS = 1095 * 86_400;
const ACTIVATION_TTL_SECS = 600;

// Stripe events we care about — see Stripe API → events.
type StripeEventType =
  | "checkout.session.completed"
  | "charge.refunded"
  | "customer.subscription.deleted";

interface StripeEvent {
  id: string;
  type: StripeEventType | string;
  data: { object: Record<string, unknown> };
}

export async function handleStripeWebhook(
  req: Request,
  env: Env,
  _ctx: ExecutionContext
): Promise<Response> {
  const sigHeader = req.headers.get("Stripe-Signature");
  if (!sigHeader) return json({ error: "missing Stripe-Signature" }, 400);

  // We need the RAW body for signature verification — JSON.parse
  // followed by re-stringify breaks the HMAC. Read text once.
  const rawBody = await req.text();

  if (!env.STRIPE_WEBHOOK_SECRET) {
    // Dev / unconfigured — refuse, don't silently accept unsigned events.
    return json({ error: "webhook signing secret not configured" }, 500);
  }
  const ok = await verifyStripeSignature(rawBody, sigHeader, env.STRIPE_WEBHOOK_SECRET);
  if (!ok) return json({ error: "invalid signature" }, 400);

  let event: StripeEvent;
  try {
    event = JSON.parse(rawBody);
  } catch {
    return json({ error: "invalid JSON" }, 400);
  }
  if (typeof event.id !== "string") {
    return json({ error: "missing event id" }, 400);
  }

  const now = Math.floor(Date.now() / 1000);
  const fresh = await recordStripeEvent(env.DB, event.id, event.type, now);
  if (!fresh) {
    // Already processed — return 200 so Stripe stops retrying.
    return json({ status: "duplicate, ignored" });
  }

  switch (event.type) {
    case "checkout.session.completed":
      await handleCheckoutCompleted(env, event.data.object, now);
      return json({ status: "license_created" });

    case "charge.refunded":
      await handleChargeRefunded(env, event.data.object, now);
      return json({ status: "license_revoked" });

    case "customer.subscription.deleted":
      // Future subscription model. For one-shot purchases this should
      // never fire. Log and ignore.
      console.log("[stripe] subscription deleted (no-op for one-shot model)");
      return json({ status: "ignored_subscription" });

    default:
      // Unhandled but valid — return 200 so Stripe doesn't retry.
      return json({ status: "ignored", type: event.type });
  }
}

async function handleCheckoutCompleted(
  env: Env,
  session: Record<string, unknown>,
  now: number
): Promise<void> {
  const sessionId = session.id as string;
  const customerEmail =
    (session.customer_details as Record<string, unknown> | undefined)?.email ??
    session.customer_email;
  const customerId = session.customer as string | undefined;

  if (typeof customerEmail !== "string" || !customerEmail.includes("@")) {
    throw new Error("checkout session missing customer email");
  }
  if (typeof sessionId !== "string") {
    throw new Error("checkout session missing id");
  }

  // Determine tier from line items / price ID. Stripe sends the
  // price_id in the line items; we configured them in wrangler.toml.
  // Some checkout configurations don't include line_items in the
  // session payload — when missing, default to annual (the more
  // common purchase) and log a warning.
  const lineItems =
    (session.line_items as Record<string, unknown> | undefined)?.data;
  let tier: "annual" | "3year" = "annual";
  if (Array.isArray(lineItems)) {
    for (const item of lineItems) {
      const priceId = (item as Record<string, unknown>)?.price as
        | { id?: string }
        | undefined;
      if (priceId?.id === env.STRIPE_PRICE_3YEAR) {
        tier = "3year";
        break;
      }
      if (priceId?.id === env.STRIPE_PRICE_ANNUAL) {
        tier = "annual";
        break;
      }
    }
  } else {
    console.warn(
      `[stripe] checkout session ${sessionId} has no line_items in payload — defaulting to annual`
    );
  }

  const validitySecs =
    tier === "3year" ? THREE_YEAR_VALIDITY_SECS : ANNUAL_VALIDITY_SECS;

  const eh = await emailHash(customerEmail);

  // Check we don't already have a license for this stripe session
  // (idempotency belt-and-braces — the stripe_events table should
  // prevent dupes, but if Stripe ever sends the same session through
  // a different event_id this catches it).
  const existingBySession = await findLicenseByStripeSession(env.DB, sessionId);
  if (existingBySession) {
    console.log(
      `[stripe] session ${sessionId} already has license ${existingBySession.license_id}, skipping`
    );
    return;
  }

  const licenseId = ulid();
  await insertLicense(env.DB, {
    license_id: licenseId,
    email_hash: eh,
    tier,
    issued_at: now,
    valid_until: now + validitySecs,
    stripe_session_id: sessionId,
    stripe_customer_id: customerId ?? null,
  });

  // Mint activation code + email magic link.
  const code = activationCode();
  await insertActivationCode(env.DB, {
    code,
    license_id: licenseId,
    created_at: now,
    expires_at: now + ACTIVATION_TTL_SECS,
  });

  const magicLink = `${env.PUBLIC_URL.replace(/\/+$/, "")}/api/activate?code=${encodeURIComponent(
    code
  )}`;

  await sendActivationEmail({
    to: customerEmail,
    magicLink,
    activationCode: code,
    tier,
    apiKey: env.RESEND_API_KEY ?? "",
    from: env.EMAIL_FROM,
  });

  await audit(
    env.DB,
    {
      event_type: "license_purchased",
      email_hash: eh,
      license_id: licenseId,
      details: { tier, stripe_session_id: sessionId },
    },
    now
  );
}

async function handleChargeRefunded(
  env: Env,
  charge: Record<string, unknown>,
  now: number
): Promise<void> {
  // Stripe's charge object references the checkout session via metadata
  // OR via the payment_intent → checkout sessions relationship. Easiest:
  // Stripe puts the original checkout session_id in the charge's
  // payment_intent.metadata when our checkout sets it. Otherwise we
  // match by customer_id as a softer fallback.
  const sessionId =
    (charge.metadata as Record<string, unknown> | undefined)?.checkout_session_id ??
    null;

  if (typeof sessionId !== "string") {
    console.warn(
      "[stripe] charge.refunded without checkout_session_id metadata — manual revoke required"
    );
    return;
  }

  const lic = await findLicenseByStripeSession(env.DB, sessionId);
  if (!lic) {
    console.warn(`[stripe] charge.refunded: no license for session ${sessionId}`);
    return;
  }
  await setLicenseStatus(env.DB, lic.license_id, "revoked");
  await audit(
    env.DB,
    {
      event_type: "license_revoked_refund",
      email_hash: lic.email_hash,
      license_id: lic.license_id,
      details: { charge_id: charge.id, stripe_session_id: sessionId },
    },
    now
  );
}

/// Verify a Stripe webhook's `Stripe-Signature` header.
///
/// Stripe signs `${timestamp}.${rawBody}` with HMAC-SHA256 keyed by
/// the endpoint's webhook secret. Header format:
///   `t=<unix>,v1=<hex>,v1=<hex>,...`  (multiple v1 = key rotation)
///
/// We verify at least one v1 matches and that the timestamp is within
/// the tolerance window (5 min — Stripe's recommended default).
async function verifyStripeSignature(
  rawBody: string,
  header: string,
  secret: string
): Promise<boolean> {
  const TOLERANCE_SECS = 300;

  const parts = header.split(",").map((p) => p.split("="));
  let timestamp: number | null = null;
  const sigs: string[] = [];
  for (const [k, v] of parts) {
    if (k === "t") timestamp = parseInt(v, 10);
    else if (k === "v1" && v) sigs.push(v);
  }
  if (timestamp === null || sigs.length === 0) return false;

  const nowSec = Math.floor(Date.now() / 1000);
  if (Math.abs(nowSec - timestamp) > TOLERANCE_SECS) return false;

  const signingInput = `${timestamp}.${rawBody}`;
  const key = await crypto.subtle.importKey(
    "raw",
    new TextEncoder().encode(secret),
    { name: "HMAC", hash: "SHA-256" },
    false,
    ["sign"]
  );
  const macBytes = new Uint8Array(
    await crypto.subtle.sign(
      "HMAC",
      key,
      new TextEncoder().encode(signingInput)
    )
  );
  const macHex = [...macBytes]
    .map((b) => b.toString(16).padStart(2, "0"))
    .join("");

  // Constant-time compare against any of the v1 signatures Stripe sent.
  for (const s of sigs) {
    if (constantTimeEqual(macHex, s)) return true;
  }
  return false;
}

function constantTimeEqual(a: string, b: string): boolean {
  if (a.length !== b.length) return false;
  let diff = 0;
  for (let i = 0; i < a.length; i++) {
    diff |= a.charCodeAt(i) ^ b.charCodeAt(i);
  }
  return diff === 0;
}
