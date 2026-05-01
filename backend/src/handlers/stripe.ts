// POST /api/stripe/webhook — Stripe webhook handler.
//
// Validates the Stripe-Signature header (HMAC-SHA256 with the webhook
// signing secret) before trusting the body. Handles three pricing
// shapes:
//   - one-time:    `lifetime` (3-year prepay) — purchase, refund.
//   - subscription: `monthly` / `annual` — purchase, period rollover,
//                   payment failure (grace), cancellation.
//
// Events:
//   checkout.session.completed         create license + send activation email
//   customer.subscription.updated      bump period_end, cancel_at_period_end, status
//   customer.subscription.deleted      revoke (subscription ended for any reason)
//   invoice.paid                       extend valid_until on rollover
//   invoice.payment_failed             status=past_due (within grace, keeps token alive)
//   charge.refunded                    revoke (one-time refunds)
//
// Idempotency: the `stripe_events` table is consulted first. On duplicate
// (Stripe retries every 5xx — and we accept once, ignore forever), we
// 200-ack without re-running the handler.

import type { Env } from "../index";
import { json } from "../index";
import {
  audit,
  findActiveLicenseByStripeCustomer,
  findLicenseByStripeSession,
  findLicenseBySubscription,
  insertActivationCode,
  insertLicense,
  recordStripeEvent,
  setLicenseStatus,
  updateLicenseFromSubscription,
} from "../db";
import { activationCode, emailHash, ulid } from "../crypto";
import { sendActivationEmail } from "../email";
// Tier subset that can come from a paid checkout (i.e. excludes "trial",
// which is provisioned via /api/trial/start, not via Stripe).
type PaidTier = "monthly" | "annual" | "lifetime";

// Default validity for the license's `valid_until` claim, by tier.
// For recurring subscriptions this is overridden by `current_period_end`
// from each `invoice.paid` — the value below is just a sensible
// fallback for the brief gap between `checkout.session.completed` and
// the first `invoice.paid` (usually seconds, sometimes minutes if
// Stripe is slow). 31 / 366 = one tier period + 1 day grace.
const TIER_VALIDITY_SECS: Record<"monthly" | "annual" | "lifetime", number> = {
  monthly: 31 * 86_400,
  annual: 366 * 86_400,
  lifetime: 1095 * 86_400, // 3 years
};

const ACTIVATION_TTL_SECS = 600;

interface StripeEvent {
  id: string;
  type: string;
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
    return json({ error: "webhook signing secret not configured" }, 500);
  }
  const ok = await verifyStripeSignature(
    rawBody,
    sigHeader,
    env.STRIPE_WEBHOOK_SECRET
  );
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

    case "customer.subscription.updated":
      await handleSubscriptionUpdated(env, event.data.object, now);
      return json({ status: "subscription_updated" });

    case "customer.subscription.deleted":
      await handleSubscriptionDeleted(env, event.data.object, now);
      return json({ status: "subscription_deleted" });

    case "invoice.paid":
      await handleInvoicePaid(env, event.data.object, now);
      return json({ status: "invoice_paid" });

    case "invoice.payment_failed":
      await handleInvoicePaymentFailed(env, event.data.object, now);
      return json({ status: "invoice_payment_failed" });

    case "charge.refunded":
      await handleChargeRefunded(env, event.data.object, now);
      return json({ status: "license_revoked" });

    default:
      // Unhandled but valid — return 200 so Stripe doesn't retry.
      return json({ status: "ignored", type: event.type });
  }
}

// ─── checkout.session.completed ─────────────────────────────────────
//
// Fires once per successful purchase, regardless of mode. We use it to
// CREATE the license + send the activation email. For subscriptions,
// subsequent `invoice.paid` events extend the existing license; we
// never create from `subscription.created`.
async function handleCheckoutCompleted(
  env: Env,
  session: Record<string, unknown>,
  now: number
): Promise<void> {
  const sessionId = session.id as string;
  const customerEmail =
    (session.customer_details as Record<string, unknown> | undefined)?.email ??
    session.customer_email;
  const customerId = (session.customer as string | undefined) ?? null;
  const subscriptionId =
    (session.subscription as string | undefined) ?? null;

  if (typeof customerEmail !== "string" || !customerEmail.includes("@")) {
    throw new Error("checkout session missing customer email");
  }
  if (typeof sessionId !== "string") {
    throw new Error("checkout session missing id");
  }

  const tier: "monthly" | "annual" | "lifetime" | null = resolveTier(env, session);
  if (!tier) {
    throw new Error(
      `checkout session ${sessionId}: cannot determine tier — missing metadata.tier and unknown price_id`
    );
  }

  const validitySecs = TIER_VALIDITY_SECS[tier];
  const eh = await emailHash(customerEmail);

  // Belt-and-braces idempotency: stripe_events should already block
  // duplicates, but if Stripe ever re-emits a session under a different
  // event_id (e.g. webhook re-signed) this catches it.
  const existing = await findLicenseByStripeSession(env.DB, sessionId);
  if (existing) {
    console.log(
      `[stripe] session ${sessionId} already has license ${existing.license_id}, skipping`
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
    stripe_customer_id: customerId,
    stripe_subscription_id: subscriptionId,
    // For subscriptions, the next `invoice.paid` (typically seconds
    // later) will overwrite this with Stripe's authoritative value.
    current_period_end: subscriptionId ? now + validitySecs : null,
  });

  // Mint activation code + email magic link.
  const code = activationCode();
  await insertActivationCode(env.DB, {
    code,
    license_id: licenseId,
    created_at: now,
    expires_at: now + ACTIVATION_TTL_SECS,
  });
  const magicLink = `${env.PUBLIC_URL}/activate?code=${encodeURIComponent(code)}`;

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
      details: {
        tier,
        stripe_session_id: sessionId,
        stripe_subscription_id: subscriptionId,
      },
    },
    now
  );
}

// ─── customer.subscription.updated ──────────────────────────────────
//
// Fires for: period rollover (current_period_end advances), cancel
// scheduled (cancel_at_period_end flips), reactivation (cancel flag
// reset), plan change. We mirror the new state into our row; the
// canonical source remains Stripe.
async function handleSubscriptionUpdated(
  env: Env,
  sub: Record<string, unknown>,
  now: number
): Promise<void> {
  const subId = sub.id as string | undefined;
  if (!subId) return;

  const periodEnd = sub.current_period_end as number | undefined;
  const cancelAt =
    typeof sub.cancel_at_period_end === "boolean"
      ? sub.cancel_at_period_end
        ? 1
        : 0
      : null;
  const stripeStatus = sub.status as string | undefined;
  // Stripe statuses we care about: active / trialing / past_due /
  // unpaid / canceled / incomplete / incomplete_expired. Map down to
  // our 3-state enum: 'active' / 'past_due' / 'revoked'.
  const ourStatus: "active" | "past_due" | "revoked" | null = (() => {
    switch (stripeStatus) {
      case "active":
      case "trialing":
        return "active";
      case "past_due":
      case "unpaid":
        return "past_due";
      case "canceled":
      case "incomplete_expired":
        return "revoked";
      default:
        return null; // leave unchanged for incomplete / unknown
    }
  })();

  const changed = await updateLicenseFromSubscription(env.DB, subId, {
    valid_until: periodEnd ?? null,
    current_period_end: periodEnd ?? null,
    cancel_at_period_end: cancelAt,
    status: ourStatus,
  });
  if (changed === 0) {
    // License doesn't exist yet — checkout.session.completed will create
    // it shortly. Stripe will resend subscription.updated when state
    // changes again so this is self-healing.
    console.warn(
      `[stripe] subscription.updated for ${subId} — no matching license yet (race with checkout.session.completed)`
    );
    return;
  }

  await audit(
    env.DB,
    {
      event_type: "subscription_updated",
      details: {
        stripe_subscription_id: subId,
        stripe_status: stripeStatus ?? null,
        cancel_at_period_end: cancelAt,
      },
    },
    now
  );
}

// ─── customer.subscription.deleted ──────────────────────────────────
//
// Subscription ended — could be voluntary cancel after period_end, or
// hard-cancel by Stripe after repeated payment_failed, or admin action.
// In all cases we revoke immediately. Stripe respects cancel_at_period_end
// internally, so by the time this fires the user is already past the
// paid window.
async function handleSubscriptionDeleted(
  env: Env,
  sub: Record<string, unknown>,
  now: number
): Promise<void> {
  const subId = sub.id as string | undefined;
  if (!subId) return;

  const lic = await findLicenseBySubscription(env.DB, subId);
  if (!lic) {
    console.warn(
      `[stripe] subscription.deleted for ${subId} — no matching license`
    );
    return;
  }

  await setLicenseStatus(env.DB, lic.license_id, "revoked");
  await audit(
    env.DB,
    {
      event_type: "subscription_deleted",
      email_hash: lic.email_hash,
      license_id: lic.license_id,
      details: { stripe_subscription_id: subId },
    },
    now
  );
}

// ─── invoice.paid ───────────────────────────────────────────────────
//
// Fires on initial purchase AND on every successful renewal. Bumps
// `valid_until` to the new `period.end` from the invoice, and lifts
// any past_due status set by an earlier `invoice.payment_failed`.
async function handleInvoicePaid(
  env: Env,
  invoice: Record<string, unknown>,
  now: number
): Promise<void> {
  const subId = invoice.subscription as string | undefined;
  if (!subId) return; // one-time invoice (e.g. lifetime) — no rollover

  // Stripe invoice.lines.data[0].period.end is the authoritative
  // period_end for this billing cycle. Fall back to top-level
  // period_end if for some reason lines isn't expanded.
  const lines = (invoice.lines as Record<string, unknown> | undefined)?.data;
  let periodEnd: number | null = null;
  if (Array.isArray(lines) && lines.length > 0) {
    const period = (lines[0] as Record<string, unknown>).period as
      | Record<string, unknown>
      | undefined;
    const end = period?.end;
    if (typeof end === "number") periodEnd = end;
  }
  if (periodEnd === null && typeof invoice.period_end === "number") {
    periodEnd = invoice.period_end;
  }
  if (periodEnd === null) {
    console.warn(`[stripe] invoice.paid for ${subId}: no period_end in payload`);
    return;
  }

  const changed = await updateLicenseFromSubscription(env.DB, subId, {
    valid_until: periodEnd,
    current_period_end: periodEnd,
    status: "active", // lifts past_due if it was set
  });
  if (changed === 0) {
    console.warn(
      `[stripe] invoice.paid for ${subId} — no matching license yet (race with checkout)`
    );
    return;
  }

  await audit(
    env.DB,
    {
      event_type: "invoice_paid",
      details: {
        stripe_subscription_id: subId,
        period_end: periodEnd,
      },
    },
    now
  );
}

// ─── invoice.payment_failed ─────────────────────────────────────────
//
// Card declined / insufficient funds / etc. Stripe retries on its own
// schedule (Smart Retries). We mark the license `past_due` so the UI
// can nudge the user to update their payment method, but we DO NOT
// shorten `valid_until` — the user still has the rest of their paid
// period to fix things. If retries ultimately fail Stripe will fire
// `customer.subscription.deleted` and we revoke.
async function handleInvoicePaymentFailed(
  env: Env,
  invoice: Record<string, unknown>,
  now: number
): Promise<void> {
  const subId = invoice.subscription as string | undefined;
  if (!subId) return;

  const changed = await updateLicenseFromSubscription(env.DB, subId, {
    status: "past_due",
  });
  if (changed === 0) {
    console.warn(
      `[stripe] invoice.payment_failed for ${subId} — no matching license`
    );
    return;
  }

  await audit(
    env.DB,
    {
      event_type: "invoice_payment_failed",
      details: { stripe_subscription_id: subId },
    },
    now
  );
}

// ─── charge.refunded ────────────────────────────────────────────────
//
// Most relevant for one-time purchases (`lifetime`). For subscriptions
// a refund is rarer (Stripe disputes / chargebacks) but if it fires for
// the full amount we still revoke.
//
// Partial refunds (amount_refunded < amount) are a no-op: a partial
// refund on a SaaS subscription is usually a goodwill credit, not a
// cancellation — the customer keeps their entitlement. We log + audit
// so the admin can manually revoke if intent was different.
//
// We look up the license by `charge.customer` since the charge object
// doesn't carry checkout_session_id. The customer-id linkage was set
// during handleCheckoutCompleted from session.customer.
async function handleChargeRefunded(
  env: Env,
  charge: Record<string, unknown>,
  now: number
): Promise<void> {
  const customerId = charge.customer as string | undefined;
  const chargeId = charge.id as string | undefined;
  const amount = (charge.amount as number | undefined) ?? 0;
  const amountRefunded = (charge.amount_refunded as number | undefined) ?? 0;
  const isFullRefund = amount > 0 && amountRefunded >= amount;

  if (typeof customerId !== "string" || customerId.length === 0) {
    console.warn(
      `[stripe] charge.refunded missing customer id (charge=${chargeId}) — manual revoke required`
    );
    return;
  }

  const lic = await findActiveLicenseByStripeCustomer(env.DB, customerId);
  if (!lic) {
    console.warn(
      `[stripe] charge.refunded: no active license for customer ${customerId}`
    );
    return;
  }

  if (!isFullRefund) {
    // Partial refund — log + audit, but DO NOT revoke. Operator decides.
    console.log(
      `[stripe] partial refund on charge ${chargeId} (refunded ${amountRefunded}/${amount}) — license ${lic.license_id} kept active`
    );
    await audit(
      env.DB,
      {
        event_type: "license_partial_refund",
        email_hash: lic.email_hash,
        license_id: lic.license_id,
        details: {
          charge_id: chargeId ?? null,
          customer_id: customerId,
          amount,
          amount_refunded: amountRefunded,
        },
      },
      now
    );
    return;
  }

  // Full refund → revoke.
  await setLicenseStatus(env.DB, lic.license_id, "revoked");
  await audit(
    env.DB,
    {
      event_type: "license_revoked_refund",
      email_hash: lic.email_hash,
      license_id: lic.license_id,
      details: {
        charge_id: chargeId ?? null,
        customer_id: customerId,
        amount,
        amount_refunded: amountRefunded,
      },
    },
    now
  );
}

// ─── helpers ────────────────────────────────────────────────────────

/// Resolve which tier a checkout session is for.
///
/// Prefers `metadata.tier` from the Payment Link config (set when we
/// create the products in F3 — Stripe persists the metadata onto every
/// session created from the link). Falls back to matching against the
/// configured price IDs in `env.STRIPE_PRICE_*` if line_items happens
/// to be expanded in the payload.
function resolveTier(env: Env, session: Record<string, unknown>): PaidTier | null {
  const fromMetadata = (session.metadata as Record<string, unknown> | undefined)
    ?.tier;
  if (
    fromMetadata === "monthly" ||
    fromMetadata === "annual" ||
    fromMetadata === "lifetime"
  ) {
    return fromMetadata;
  }

  const lineItems =
    (session.line_items as Record<string, unknown> | undefined)?.data;
  if (Array.isArray(lineItems)) {
    for (const item of lineItems) {
      const price = (item as Record<string, unknown>)?.price as
        | { id?: string }
        | undefined;
      if (price?.id === env.STRIPE_PRICE_MONTHLY) return "monthly";
      if (price?.id === env.STRIPE_PRICE_ANNUAL) return "annual";
      if (price?.id === env.STRIPE_PRICE_LIFETIME) return "lifetime";
    }
  }
  return null;
}

/// Verify a Stripe webhook's `Stripe-Signature` header.
///
/// Stripe signs `${timestamp}.${rawBody}` with HMAC-SHA256 keyed by
/// the endpoint's webhook secret. Header format:
///   `t=<unix>,v1=<hex>,v1=<hex>,...`  (multiple v1 = key rotation)
///
/// We verify at least one v1 matches and that the timestamp is within
/// the tolerance window (5 min — Stripe's recommended default).
export async function verifyStripeSignature(
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
