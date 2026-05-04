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
  findActiveLicenseByEmail,
  findActiveLicenseByStripeCustomer,
  findLicenseByStripeSession,
  findLicenseBySubscription,
  findRecentUnconsumedActivationCode,
  insertActivationCode,
  insertLicense,
  recordStripeEvent,
  setLicenseStatus,
  updateLicenseFromSubscription,
  updateLicenseTierBySubscription,
  upgradeLicenseToLifetime,
  type LicenseRow,
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
/// Same dedup window as trial.ts — re-use an unconsumed code created
/// within the last MAGIC_LINK_DEDUP_SECS for this license instead of
/// minting a new one. Two webhook re-fires within 5 min (Stripe Smart
/// Retry) shouldn't email the user twice with two different codes.
const MAGIC_LINK_DEDUP_SECS = 300;

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

    case "refund.created":
      // Modern refund event (2024+). Stripe is nudging integrations off
      // `charge.refunded` toward `refund.created` for refund signals.
      // Adopt now so we're aligned with where Stripe is going; keep
      // the legacy handler below as defense until refund.created has a
      // few weeks of clean prod runs.
      await handleRefundCreated(env, event.data.object, now);
      return json({ status: "license_revoked" });

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

  // ── Duplicate-purchase gate ────────────────────────────────────────
  //
  // Same email already has an active *paid* license. Three outcomes,
  // depending on the relation between existing and new tier:
  //
  //   existing=lifetime           → BLOCK (lifetime is the ceiling).
  //   existing=monthly|annual,
  //     new=lifetime              → IN-PLACE UPGRADE (lifetime "wins":
  //                                  cancel old sub, mutate row to
  //                                  lifetime, send magic link).
  //   existing=monthly|annual,
  //     new=monthly|annual        → BLOCK (duplicate purchase — user
  //                                  re-clicked Buy after a webhook
  //                                  hiccup or used a bookmarked
  //                                  Payment Link).
  //
  // existing=trial → fall through to the create path below: paying any
  // tier while on trial is the legitimate trial→paid upgrade flow.
  //
  // BLOCK = cancel the new Stripe subscription via the API (refund
  // automatic if Stripe is within the immediate-refund window), send a
  // magic link for the EXISTING license so the user can activate this
  // device, audit the event. License row count stays at one per email.
  const existingForEmail = await findActiveLicenseByEmail(env.DB, eh);
  if (existingForEmail && existingForEmail.tier !== "trial") {
    const existingTier = existingForEmail.tier as PaidTier;

    if (existingTier === "lifetime") {
      await blockDuplicatePurchase(env, {
        eh,
        customerEmail,
        attemptedTier: tier,
        attemptedSessionId: sessionId,
        attemptedSubscriptionId: subscriptionId,
        existingLicense: existingForEmail,
        reason: "lifetime_already_active",
        now,
      });
      return;
    }

    if (tier === "lifetime") {
      // Lifetime supersedes the active sub. Cancel the recurring sub
      // (best-effort), mutate the existing license row, send a magic
      // link so the client picks up a token signed with tier=lifetime.
      if (existingForEmail.stripe_subscription_id) {
        await cancelStripeSubscriptionSafe(
          env,
          existingForEmail.stripe_subscription_id,
          "superseded by lifetime purchase"
        );
      }
      const lifetimeValidUntil = now + TIER_VALIDITY_SECS.lifetime;
      await upgradeLicenseToLifetime(env.DB, existingForEmail.license_id, {
        new_valid_until: lifetimeValidUntil,
        new_session_id: sessionId,
        new_customer_id: customerId,
      });
      await sendDuplicatePurchaseMagicLink(env, {
        licenseId: existingForEmail.license_id,
        customerEmail,
        tier: "lifetime",
        now,
      });
      await audit(
        env.DB,
        {
          event_type: "license_upgraded_to_lifetime",
          email_hash: eh,
          license_id: existingForEmail.license_id,
          details: {
            previous_tier: existingTier,
            new_session_id: sessionId,
            old_subscription_cancelled: !!existingForEmail.stripe_subscription_id,
          },
        },
        now
      );
      return;
    }

    // Monthly/annual + monthly/annual = duplicate. Cancel the new sub
    // (the existing sub stays — that's the one the user already has).
    await blockDuplicatePurchase(env, {
      eh,
      customerEmail,
      attemptedTier: tier,
      attemptedSessionId: sessionId,
      attemptedSubscriptionId: subscriptionId,
      existingLicense: existingForEmail,
      reason: "duplicate_subscription",
      now,
    });
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

  // Catch Resend failures so we still record the license + activation
  // code in DB. Without this catch, a transient Resend error would
  // throw 500 and the webhook handler aborts AFTER the row is written
  // — Stripe retries, and our idempotency key blocks the retry, so
  // the user keeps a paid license with no magic link to consume. With
  // the catch, we log + continue; the user can re-issue from the app.
  try {
    await sendActivationEmail({
      to: customerEmail,
      magicLink,
      activationCode: code,
      tier,
      apiKey: env.RESEND_API_KEY ?? "",
      from: env.EMAIL_FROM,
    });
  } catch (err) {
    const msg = err instanceof Error ? err.message : String(err);
    console.error(`[stripe] sendActivationEmail failed (license=${licenseId}): ${msg}`);
  }

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

  // Plan change: detect a price flip on items[0] and mirror the new
  // tier into the license row. This is the path /api/plan-change uses
  // — the worker calls Stripe to flip the price, Stripe fires
  // subscription.updated with the new price, here we propagate the
  // tier change to D1 so the next /api/refresh returns a token signed
  // with the new tier.
  const items = sub.items as { data?: Array<{ price?: { id?: string } }> } | undefined;
  const newPriceId = items?.data?.[0]?.price?.id;
  if (typeof newPriceId === "string" && newPriceId.length > 0) {
    let newTier: "monthly" | "annual" | null = null;
    if (newPriceId === env.STRIPE_PRICE_MONTHLY) newTier = "monthly";
    else if (newPriceId === env.STRIPE_PRICE_ANNUAL) newTier = "annual";
    // Lifetime is one-time, not a sub — would never appear here.
    if (newTier) {
      const tierChanged = await updateLicenseTierBySubscription(
        env.DB,
        subId,
        newTier
      );
      if (tierChanged > 0) {
        await audit(
          env.DB,
          {
            event_type: "subscription_tier_changed",
            details: {
              stripe_subscription_id: subId,
              new_tier: newTier,
              new_price_id: newPriceId,
            },
          },
          now
        );
      }
    }
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

// ─── refund.created (modern refund event) ───────────────────────────
//
// Refund object payload carries `id`, `amount` (THIS refund's amount),
// `currency`, `charge` (charge id), `customer`, and `status`. We need
// to know whether this refund completes the charge (full → revoke) or
// is a partial-credit (no-op, audit only). The refund event itself
// only tells us about THIS refund chunk; we have to fetch the parent
// charge from the Stripe API to compare `charge.amount` to the
// post-refund `charge.amount_refunded`.
//
// One Stripe API call per refund is acceptable: refunds are rare and
// the call latency is ~50ms. STRIPE_SECRET_KEY required (already
// required by the duplicate-purchase gate).
async function handleRefundCreated(
  env: Env,
  refund: Record<string, unknown>,
  now: number
): Promise<void> {
  const refundId = refund.id as string | undefined;
  const chargeId = refund.charge as string | undefined;
  const refundCustomer = refund.customer as string | undefined;
  const refundStatus = refund.status as string | undefined;
  const refundAmount = (refund.amount as number | undefined) ?? 0;

  // Pending / failed / canceled refunds aren't actually money returned.
  // We only act on succeeded.
  if (refundStatus !== "succeeded") {
    console.log(
      `[stripe] refund.created ${refundId} status=${refundStatus} — no action`
    );
    return;
  }
  if (typeof chargeId !== "string" || chargeId.length === 0) {
    console.warn(
      `[stripe] refund.created ${refundId} missing charge id — manual revoke required`
    );
    return;
  }

  // Fetch the parent charge to learn full vs partial.
  const charge = await fetchStripeCharge(env, chargeId);
  if (!charge) {
    // Couldn't fetch — defensive: log loudly. Operator can replay the
    // event from the Stripe dashboard once STRIPE_SECRET_KEY is set or
    // the network blip clears.
    console.error(
      `[stripe] refund.created ${refundId}: could not fetch charge ${chargeId} — license NOT touched`
    );
    return;
  }
  const totalAmount = (charge.amount as number | undefined) ?? 0;
  const amountRefunded = (charge.amount_refunded as number | undefined) ?? 0;
  const isFullRefund = totalAmount > 0 && amountRefunded >= totalAmount;

  // Customer linkage: refund.customer is the most direct, but Stripe
  // sometimes omits it on Refund objects. Fall back to charge.customer.
  const customerId =
    refundCustomer ?? (charge.customer as string | undefined);
  if (typeof customerId !== "string" || customerId.length === 0) {
    console.warn(
      `[stripe] refund.created ${refundId}: no customer linkage on refund or charge`
    );
    return;
  }

  const lic = await findActiveLicenseByStripeCustomer(env.DB, customerId);
  if (!lic) {
    // Possibly the legacy charge.refunded handler already revoked it
    // (status is now != 'active'). In that case this is the second
    // half of a Stripe-emits-both pair and we no-op cleanly. Nothing
    // to do, no log noise.
    console.log(
      `[stripe] refund.created ${refundId}: no active license for customer ${customerId} (already revoked or never linked)`
    );
    return;
  }

  if (!isFullRefund) {
    console.log(
      `[stripe] refund.created ${refundId}: partial (${amountRefunded}/${totalAmount}) — license ${lic.license_id} kept active`
    );
    await audit(
      env.DB,
      {
        event_type: "license_partial_refund",
        email_hash: lic.email_hash,
        license_id: lic.license_id,
        details: {
          refund_id: refundId ?? null,
          charge_id: chargeId,
          customer_id: customerId,
          amount: totalAmount,
          amount_refunded: amountRefunded,
          this_refund_amount: refundAmount,
          source: "refund.created",
        },
      },
      now
    );
    return;
  }

  await setLicenseStatus(env.DB, lic.license_id, "revoked");
  await audit(
    env.DB,
    {
      event_type: "license_revoked_refund",
      email_hash: lic.email_hash,
      license_id: lic.license_id,
      details: {
        refund_id: refundId ?? null,
        charge_id: chargeId,
        customer_id: customerId,
        amount: totalAmount,
        amount_refunded: amountRefunded,
        this_refund_amount: refundAmount,
        source: "refund.created",
      },
    },
    now
  );
}

/// GET /v1/charges/:id from Stripe API. Returns the parsed charge
/// object on success or null on any error (network, auth, missing
/// secret, non-2xx). Errors are logged so the operator sees missing
/// STRIPE_SECRET_KEY as the obvious cause.
async function fetchStripeCharge(
  env: Env,
  chargeId: string
): Promise<Record<string, unknown> | null> {
  if (!env.STRIPE_SECRET_KEY) {
    console.error(
      `[stripe] cannot fetch charge ${chargeId} — STRIPE_SECRET_KEY env var missing. ` +
      `Set it with \`wrangler secret put STRIPE_SECRET_KEY --env <env>\`.`
    );
    return null;
  }
  try {
    const r = await fetch(
      `https://api.stripe.com/v1/charges/${encodeURIComponent(chargeId)}`,
      {
        headers: { Authorization: `Bearer ${env.STRIPE_SECRET_KEY}` },
      }
    );
    if (!r.ok) {
      const body = await r.text();
      console.error(
        `[stripe] fetch charge ${chargeId} failed: HTTP ${r.status} body=${body.slice(0, 200)}`
      );
      return null;
    }
    return (await r.json()) as Record<string, unknown>;
  } catch (e) {
    console.error(`[stripe] fetch charge ${chargeId} threw:`, e);
    return null;
  }
}

// ─── duplicate-purchase gate helpers ────────────────────────────────

/// Common path for both "lifetime already exists" and "duplicate sub"
/// blocks. Cancels the new Stripe subscription via the API (best-effort
/// — refund is automatic if Stripe is within the immediate-refund
/// window), sends the user a magic link for their EXISTING license so
/// they can activate the device they probably meant to add, audit logs.
async function blockDuplicatePurchase(
  env: Env,
  args: {
    eh: string;
    customerEmail: string;
    attemptedTier: PaidTier;
    attemptedSessionId: string;
    attemptedSubscriptionId: string | null;
    existingLicense: LicenseRow;
    reason: "lifetime_already_active" | "duplicate_subscription";
    now: number;
  }
): Promise<void> {
  let cancelled = false;
  if (args.attemptedSubscriptionId) {
    cancelled = await cancelStripeSubscriptionSafe(
      env,
      args.attemptedSubscriptionId,
      `duplicate purchase blocked: ${args.reason}`
    );
  }
  // Lifetime is one-time — no sub to cancel. We log and still send the
  // magic link, but a manual refund via Stripe dashboard may be needed.
  // Surfaces in the audit row's `details.attempted_subscription_id`
  // (null for lifetime).

  await sendDuplicatePurchaseMagicLink(env, {
    licenseId: args.existingLicense.license_id,
    customerEmail: args.customerEmail,
    tier: args.existingLicense.tier as PaidTier,
    now: args.now,
  });

  await audit(
    env.DB,
    {
      event_type: "duplicate_purchase_blocked",
      email_hash: args.eh,
      license_id: args.existingLicense.license_id,
      details: {
        reason: args.reason,
        attempted_session_id: args.attemptedSessionId,
        attempted_tier: args.attemptedTier,
        attempted_subscription_id: args.attemptedSubscriptionId,
        existing_tier: args.existingLicense.tier,
        cancelled_attempted_subscription: cancelled,
      },
    },
    args.now
  );
}

/// Mint an activation code for an existing license + send the magic
/// link via Resend. Used by the duplicate-purchase gate to give the
/// user a working path forward (activate this device on the license
/// they already have) instead of leaving them confused.
async function sendDuplicatePurchaseMagicLink(
  env: Env,
  args: {
    licenseId: string;
    customerEmail: string;
    tier: PaidTier;
    now: number;
  }
): Promise<void> {
  // Dedup window — see MAGIC_LINK_DEDUP_SECS comment.
  const recent = await findRecentUnconsumedActivationCode(
    env.DB,
    args.licenseId,
    args.now - MAGIC_LINK_DEDUP_SECS
  );
  const code = recent?.code ?? activationCode();
  if (!recent) {
    await insertActivationCode(env.DB, {
      code,
      license_id: args.licenseId,
      created_at: args.now,
      expires_at: args.now + ACTIVATION_TTL_SECS,
    });
  }
  const magicLink = `${env.PUBLIC_URL}/activate?code=${encodeURIComponent(code)}`;
  try {
    await sendActivationEmail({
      to: args.customerEmail,
      magicLink,
      activationCode: code,
      tier: args.tier,
      apiKey: env.RESEND_API_KEY ?? "",
      from: env.EMAIL_FROM,
    });
  } catch (err) {
    const msg = err instanceof Error ? err.message : String(err);
    console.error(`[stripe] dup-purchase magic-link send failed (license=${args.licenseId}): ${msg}`);
  }
}

/// Cancel a Stripe subscription via the REST API. Returns true on
/// success, false on any error (network, auth, missing secret). Errors
/// are logged loudly because a silent failure here means the user keeps
/// being charged for a sub we promised to cancel.
///
/// Uses immediate cancellation (DELETE /v1/subscriptions/:id) rather
/// than `cancel_at_period_end=true` because the duplicate is, by
/// definition, money the user didn't intend to spend — they shouldn't
/// have to wait until period end to stop paying.
async function cancelStripeSubscriptionSafe(
  env: Env,
  subscriptionId: string,
  reason: string
): Promise<boolean> {
  if (!env.STRIPE_SECRET_KEY) {
    console.error(
      `[stripe] CANNOT cancel ${subscriptionId} — STRIPE_SECRET_KEY env var missing. ` +
      `User will keep being charged. Set the secret with ` +
      `\`wrangler secret put STRIPE_SECRET_KEY --env <env>\`. Reason: ${reason}`
    );
    return false;
  }
  try {
    const r = await fetch(
      `https://api.stripe.com/v1/subscriptions/${encodeURIComponent(subscriptionId)}`,
      {
        method: "DELETE",
        headers: {
          Authorization: `Bearer ${env.STRIPE_SECRET_KEY}`,
        },
      }
    );
    if (!r.ok) {
      const body = await r.text();
      console.error(
        `[stripe] cancel ${subscriptionId} failed: HTTP ${r.status} body=${body.slice(0, 200)}`
      );
      return false;
    }
    console.log(
      `[stripe] cancelled subscription ${subscriptionId} (reason: ${reason})`
    );
    return true;
  } catch (e) {
    console.error(`[stripe] cancel ${subscriptionId} threw:`, e);
    return false;
  }
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
