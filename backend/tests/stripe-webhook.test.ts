// End-to-end tests of /api/stripe/webhook against an in-memory D1 mock.
// Exercises the production stripe handler — same code path Stripe will
// hit in production, with real HMAC sigs the handler verifies.

import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import { handleStripeWebhook } from "../src/handlers/stripe";
import { emailHash } from "../src/crypto";
import { emptyState, makeMockDB, type MockState } from "./_d1-mock";
import type { Env } from "../src/index";

const SECRET = "whsec_test_secret";

function makeEnv(state: MockState, opts: { stripeSecret?: string } = {}): Env {
  return {
    DB: makeMockDB(state) as unknown as D1Database,
    PUBLIC_URL: "http://localhost:8787",
    EMAIL_FROM: "Dimmy <hello@dimmy.app>",
    STRIPE_PRICE_MONTHLY: "price_monthly_test",
    STRIPE_PRICE_ANNUAL: "price_annual_test",
    STRIPE_PRICE_LIFETIME: "price_lifetime_test",
    DIMMY_LICENSE_PRIVKEY: "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
    DIMMY_LICENSE_PUBKEY: "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
    STRIPE_WEBHOOK_SECRET: SECRET,
    // Empty default keeps existing tests Stripe-API-free; gate tests
    // override to assert cancel-sub fetch was issued.
    STRIPE_SECRET_KEY: opts.stripeSecret ?? "",
    RESEND_API_KEY: "", // dev-fallback (console.log)
  };
}

async function signedRequest(body: string, secret = SECRET): Promise<Request> {
  const ts = Math.floor(Date.now() / 1000);
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
      new TextEncoder().encode(`${ts}.${body}`)
    )
  );
  const macHex = [...macBytes]
    .map((b) => b.toString(16).padStart(2, "0"))
    .join("");
  return new Request("http://localhost/api/stripe/webhook", {
    method: "POST",
    headers: {
      "Content-Type": "application/json",
      "Stripe-Signature": `t=${ts},v1=${macHex}`,
    },
    body,
  });
}

const ctx = {} as ExecutionContext;

describe("/api/stripe/webhook", () => {
  test("400 on missing signature header", async () => {
    const state = emptyState();
    const req = new Request("http://localhost/api/stripe/webhook", {
      method: "POST",
      body: "{}",
    });
    const resp = await handleStripeWebhook(req, makeEnv(state), ctx);
    expect(resp.status).toBe(400);
    const j = (await resp.json()) as { error: string };
    expect(j.error).toContain("Stripe-Signature");
  });

  test("400 on invalid signature", async () => {
    const state = emptyState();
    const req = new Request("http://localhost/api/stripe/webhook", {
      method: "POST",
      headers: {
        "Content-Type": "application/json",
        "Stripe-Signature": "t=0,v1=00",
      },
      body: '{"id":"evt_x","type":"checkout.session.completed","data":{"object":{}}}',
    });
    const resp = await handleStripeWebhook(req, makeEnv(state), ctx);
    expect(resp.status).toBe(400);
  });

  test("checkout.session.completed (lifetime, one-time) creates a license", async () => {
    const state = emptyState();
    const body = JSON.stringify({
      id: "evt_1",
      type: "checkout.session.completed",
      data: {
        object: {
          id: "cs_test_lifetime_1",
          mode: "payment",
          customer: "cus_test_1",
          customer_details: { email: "alice@example.com" },
          metadata: { tier: "lifetime" },
        },
      },
    });
    const resp = await handleStripeWebhook(
      await signedRequest(body),
      makeEnv(state),
      ctx
    );
    expect(resp.status).toBe(200);
    expect(state.licenses.size).toBe(1);
    const lic = [...state.licenses.values()][0];
    expect(lic.tier).toBe("lifetime");
    expect(lic.stripe_session_id).toBe("cs_test_lifetime_1");
    expect(lic.stripe_subscription_id).toBeNull();
    // Validity ≈ 1095 days from now
    const validFor = (lic.valid_until as number) - (lic.issued_at as number);
    expect(validFor).toBeGreaterThan(1090 * 86400);
    expect(validFor).toBeLessThan(1100 * 86400);
    // Activation code minted
    expect(state.activation_codes.size).toBe(1);
  });

  test("checkout.session.completed (monthly subscription) sets stripe_subscription_id", async () => {
    const state = emptyState();
    const body = JSON.stringify({
      id: "evt_2",
      type: "checkout.session.completed",
      data: {
        object: {
          id: "cs_test_monthly_1",
          mode: "subscription",
          customer: "cus_test_2",
          subscription: "sub_test_1",
          customer_details: { email: "bob@example.com" },
          metadata: { tier: "monthly" },
        },
      },
    });
    const resp = await handleStripeWebhook(
      await signedRequest(body),
      makeEnv(state),
      ctx
    );
    expect(resp.status).toBe(200);
    const lic = [...state.licenses.values()][0];
    expect(lic.tier).toBe("monthly");
    expect(lic.stripe_subscription_id).toBe("sub_test_1");
    expect(lic.current_period_end).not.toBeNull();
  });

  test("duplicate event id → idempotent no-op", async () => {
    const state = emptyState();
    const body = JSON.stringify({
      id: "evt_dup",
      type: "checkout.session.completed",
      data: {
        object: {
          id: "cs_dup",
          mode: "payment",
          customer: "cus",
          customer_details: { email: "x@y.com" },
          metadata: { tier: "lifetime" },
        },
      },
    });
    const env = makeEnv(state);
    const r1 = await handleStripeWebhook(await signedRequest(body), env, ctx);
    const r2 = await handleStripeWebhook(await signedRequest(body), env, ctx);
    expect(r1.status).toBe(200);
    expect(r2.status).toBe(200);
    expect(state.licenses.size).toBe(1); // not 2
    const j2 = (await r2.json()) as { status: string };
    expect(j2.status).toContain("duplicate");
  });

  test("invoice.paid extends valid_until + lifts past_due", async () => {
    // Seed an existing license whose status was knocked to past_due.
    const state = emptyState();
    state.licenses.set("lic_x", {
      license_id: "lic_x",
      email_hash: "eh",
      tier: "monthly",
      issued_at: 1000,
      valid_until: 1000 + 31 * 86400,
      max_devices: 5,
      status: "past_due",
      stripe_session_id: "cs_x",
      stripe_customer_id: "cus_x",
      stripe_subscription_id: "sub_x",
      current_period_end: 1000 + 31 * 86400,
      cancel_at_period_end: 0,
    });
    const newPeriodEnd = 1000 + 62 * 86400; // next monthly cycle
    const body = JSON.stringify({
      id: "evt_inv_1",
      type: "invoice.paid",
      data: {
        object: {
          subscription: "sub_x",
          lines: { data: [{ period: { end: newPeriodEnd } }] },
        },
      },
    });
    const resp = await handleStripeWebhook(
      await signedRequest(body),
      makeEnv(state),
      ctx
    );
    expect(resp.status).toBe(200);
    const lic = state.licenses.get("lic_x")!;
    expect(lic.valid_until).toBe(newPeriodEnd);
    expect(lic.current_period_end).toBe(newPeriodEnd);
    expect(lic.status).toBe("active"); // lifted from past_due
  });

  test("invoice.payment_failed → past_due (no validity change)", async () => {
    const state = emptyState();
    const validUntil = 1000 + 31 * 86400;
    state.licenses.set("lic_y", {
      license_id: "lic_y",
      email_hash: "eh",
      tier: "monthly",
      issued_at: 1000,
      valid_until: validUntil,
      max_devices: 5,
      status: "active",
      stripe_session_id: "cs_y",
      stripe_customer_id: "cus_y",
      stripe_subscription_id: "sub_y",
      current_period_end: validUntil,
      cancel_at_period_end: 0,
    });
    const body = JSON.stringify({
      id: "evt_inv_fail",
      type: "invoice.payment_failed",
      data: { object: { subscription: "sub_y" } },
    });
    const resp = await handleStripeWebhook(
      await signedRequest(body),
      makeEnv(state),
      ctx
    );
    expect(resp.status).toBe(200);
    const lic = state.licenses.get("lic_y")!;
    expect(lic.status).toBe("past_due");
    // valid_until unchanged — user still has the rest of the paid period
    expect(lic.valid_until).toBe(validUntil);
  });

  test("customer.subscription.deleted → revoke", async () => {
    const state = emptyState();
    state.licenses.set("lic_z", {
      license_id: "lic_z",
      email_hash: "eh",
      tier: "annual",
      issued_at: 1000,
      valid_until: 1000 + 366 * 86400,
      max_devices: 5,
      status: "active",
      stripe_session_id: "cs_z",
      stripe_customer_id: "cus_z",
      stripe_subscription_id: "sub_z",
      current_period_end: 1000 + 366 * 86400,
      cancel_at_period_end: 0,
    });
    const body = JSON.stringify({
      id: "evt_sub_del",
      type: "customer.subscription.deleted",
      data: { object: { id: "sub_z" } },
    });
    const resp = await handleStripeWebhook(
      await signedRequest(body),
      makeEnv(state),
      ctx
    );
    expect(resp.status).toBe(200);
    expect(state.licenses.get("lic_z")!.status).toBe("revoked");
  });

  test("customer.subscription.updated mirrors period_end + cancel flag", async () => {
    const state = emptyState();
    state.licenses.set("lic_u", {
      license_id: "lic_u",
      email_hash: "eh",
      tier: "annual",
      issued_at: 1000,
      valid_until: 1000 + 366 * 86400,
      max_devices: 5,
      status: "active",
      stripe_session_id: "cs_u",
      stripe_customer_id: "cus_u",
      stripe_subscription_id: "sub_u",
      current_period_end: 1000 + 366 * 86400,
      cancel_at_period_end: 0,
    });
    const newEnd = 1000 + 400 * 86400;
    const body = JSON.stringify({
      id: "evt_sub_upd",
      type: "customer.subscription.updated",
      data: {
        object: {
          id: "sub_u",
          current_period_end: newEnd,
          cancel_at_period_end: true,
          status: "active",
        },
      },
    });
    const resp = await handleStripeWebhook(
      await signedRequest(body),
      makeEnv(state),
      ctx
    );
    expect(resp.status).toBe(200);
    const lic = state.licenses.get("lic_u")!;
    expect(lic.current_period_end).toBe(newEnd);
    expect(lic.cancel_at_period_end).toBe(1);
    expect(lic.valid_until).toBe(newEnd);
  });

  test("unknown event type → 200 ignored (no Stripe retries)", async () => {
    const state = emptyState();
    const body = JSON.stringify({
      id: "evt_unknown",
      type: "customer.created",
      data: { object: {} },
    });
    const resp = await handleStripeWebhook(
      await signedRequest(body),
      makeEnv(state),
      ctx
    );
    expect(resp.status).toBe(200);
    const j = (await resp.json()) as { status: string };
    expect(j.status).toBe("ignored");
  });

  test("checkout without metadata.tier falls back to price_id match", async () => {
    const state = emptyState();
    const body = JSON.stringify({
      id: "evt_priceid",
      type: "checkout.session.completed",
      data: {
        object: {
          id: "cs_priceid",
          mode: "subscription",
          customer: "cus",
          subscription: "sub_priceid",
          customer_details: { email: "c@d.com" },
          line_items: { data: [{ price: { id: "price_annual_test" } }] },
        },
      },
    });
    const resp = await handleStripeWebhook(
      await signedRequest(body),
      makeEnv(state),
      ctx
    );
    expect(resp.status).toBe(200);
    expect([...state.licenses.values()][0].tier).toBe("annual");
  });

  test("checkout with no tier hint → throws (caught by index.ts → 500)", async () => {
    // The handler throws; index.ts's outer try/catch converts the
    // throw into a 500 envelope. Here we test the throw directly.
    const state = emptyState();
    const body = JSON.stringify({
      id: "evt_notier",
      type: "checkout.session.completed",
      data: {
        object: {
          id: "cs_notier",
          mode: "payment",
          customer: "cus",
          customer_details: { email: "e@f.com" },
        },
      },
    });
    const env = makeEnv(state);
    const req = await signedRequest(body);
    await expect(handleStripeWebhook(req, env, ctx)).rejects.toThrow(
      /cannot determine tier/
    );
    expect(state.licenses.size).toBe(0);
  });

  test("customer.subscription.updated UNCANCEL clears cancel_at_period_end flag", async () => {
    // User changes their mind: cancels then reactivates from Customer
    // Portal. Stripe fires subscription.updated again with cancel_at_
    // period_end: false. Our COALESCE-based UPDATE writes the new 0
    // (not NULL), so the flag clears + UI subtitle disappears next refresh.
    const state = emptyState();
    state.licenses.set("lic_uncxl", {
      license_id: "lic_uncxl", email_hash: "eh", tier: "annual",
      issued_at: 1000, valid_until: 1000 + 366 * 86400,
      max_devices: 5, status: "active",
      stripe_session_id: "cs", stripe_customer_id: "cus",
      stripe_subscription_id: "sub_uncxl",
      current_period_end: 1000 + 366 * 86400,
      cancel_at_period_end: 1, // previously cancelled
    });
    const body = JSON.stringify({
      id: "evt_uncxl",
      type: "customer.subscription.updated",
      data: {
        object: {
          id: "sub_uncxl",
          status: "active",
          current_period_end: 1000 + 366 * 86400,
          cancel_at_period_end: false, // uncancel
        },
      },
    });
    const resp = await handleStripeWebhook(
      await signedRequest(body), makeEnv(state), ctx
    );
    expect(resp.status).toBe(200);
    const lic = state.licenses.get("lic_uncxl")!;
    expect(lic.cancel_at_period_end).toBe(0);
    expect(lic.status).toBe("active");
  });

  test("subscription.updated past_due → active recovers status", async () => {
    // Card fails → past_due. Card succeeds on retry → back to active.
    const state = emptyState();
    state.licenses.set("lic_recover", {
      license_id: "lic_recover", email_hash: "eh", tier: "monthly",
      issued_at: 1000, valid_until: 1000 + 30 * 86400,
      max_devices: 5, status: "past_due",
      stripe_session_id: "cs", stripe_customer_id: "cus",
      stripe_subscription_id: "sub_rec",
      current_period_end: 1000 + 30 * 86400,
      cancel_at_period_end: 0,
    });
    const body = JSON.stringify({
      id: "evt_rec",
      type: "customer.subscription.updated",
      data: {
        object: {
          id: "sub_rec",
          status: "active", // back from past_due
          current_period_end: 1000 + 30 * 86400,
          cancel_at_period_end: false,
        },
      },
    });
    const resp = await handleStripeWebhook(
      await signedRequest(body), makeEnv(state), ctx
    );
    expect(resp.status).toBe(200);
    expect(state.licenses.get("lic_recover")!.status).toBe("active");
  });

  test("subscription.updated for unknown sub_id → 200 (self-heals on next event)", async () => {
    // Stripe occasionally fires subscription.updated before our
    // checkout.session.completed handler has had a chance to insert
    // the license. We log + 200 so Stripe doesn't retry to oblivion;
    // the next event after the license exists will reconcile.
    const state = emptyState();
    const body = JSON.stringify({
      id: "evt_orphan_sub",
      type: "customer.subscription.updated",
      data: {
        object: {
          id: "sub_does_not_exist",
          status: "active",
          current_period_end: 1234567890,
          cancel_at_period_end: false,
        },
      },
    });
    const resp = await handleStripeWebhook(
      await signedRequest(body), makeEnv(state), ctx
    );
    expect(resp.status).toBe(200);
  });

  test("charge.refunded full → license revoked + audit logged", async () => {
    const state = emptyState();
    state.licenses.set("lic_full_refund", {
      license_id: "lic_full_refund",
      email_hash: "eh",
      tier: "lifetime",
      issued_at: 1000,
      valid_until: 1000 + 1095 * 86400,
      max_devices: 5,
      status: "active",
      stripe_session_id: "cs_full",
      stripe_customer_id: "cus_full",
      stripe_subscription_id: null,
      current_period_end: null,
      cancel_at_period_end: 0,
    });
    const body = JSON.stringify({
      id: "evt_full_refund",
      type: "charge.refunded",
      data: {
        object: {
          id: "ch_full",
          customer: "cus_full",
          amount: 3900,
          amount_refunded: 3900,
        },
      },
    });
    const resp = await handleStripeWebhook(
      await signedRequest(body),
      makeEnv(state),
      ctx
    );
    expect(resp.status).toBe(200);
    expect(state.licenses.get("lic_full_refund")!.status).toBe("revoked");
    // Audit row was written.
    expect(state.audit_log.length).toBeGreaterThan(0);
    const lastAudit = state.audit_log[state.audit_log.length - 1];
    expect(lastAudit.event_type).toBe("license_revoked_refund");
  });

  test("charge.refunded partial → license stays active, audit logs partial", async () => {
    // SaaS partial refunds are usually goodwill credits — entitlement
    // remains. Operator can manually revoke if intent was different.
    const state = emptyState();
    state.licenses.set("lic_partial", {
      license_id: "lic_partial",
      email_hash: "eh",
      tier: "annual",
      issued_at: 1000,
      valid_until: 1000 + 366 * 86400,
      max_devices: 5,
      status: "active",
      stripe_session_id: "cs_p",
      stripe_customer_id: "cus_p",
      stripe_subscription_id: "sub_p",
      current_period_end: 1000 + 366 * 86400,
      cancel_at_period_end: 0,
    });
    const body = JSON.stringify({
      id: "evt_partial",
      type: "charge.refunded",
      data: {
        object: {
          id: "ch_p",
          customer: "cus_p",
          amount: 1900, // €19 in cents
          amount_refunded: 500, // €5 partial credit
        },
      },
    });
    const resp = await handleStripeWebhook(
      await signedRequest(body),
      makeEnv(state),
      ctx
    );
    expect(resp.status).toBe(200);
    // License must remain active.
    expect(state.licenses.get("lic_partial")!.status).toBe("active");
    // Partial audit was written.
    const partialAudits = state.audit_log.filter(
      (a) => a.event_type === "license_partial_refund"
    );
    expect(partialAudits.length).toBe(1);
  });

  test("charge.refunded for unknown customer → no-op (logged warning)", async () => {
    // Stripe occasionally fires charge.refunded for orphaned / migrated
    // accounts. We tolerate by no-op'ing rather than throwing — the
    // event has been logged already and a 500 here would just trigger
    // pointless Stripe retries.
    const state = emptyState();
    const body = JSON.stringify({
      id: "evt_orphan",
      type: "charge.refunded",
      data: {
        object: {
          id: "ch_orphan",
          customer: "cus_does_not_exist",
          amount: 1000,
          amount_refunded: 1000,
        },
      },
    });
    const resp = await handleStripeWebhook(
      await signedRequest(body),
      makeEnv(state),
      ctx
    );
    expect(resp.status).toBe(200);
    expect(state.licenses.size).toBe(0);
  });

  test("charge.refunded missing customer → no-op (logged warning)", async () => {
    const state = emptyState();
    const body = JSON.stringify({
      id: "evt_no_customer",
      type: "charge.refunded",
      data: {
        object: { id: "ch_x", amount: 100, amount_refunded: 100 },
      },
    });
    const resp = await handleStripeWebhook(
      await signedRequest(body),
      makeEnv(state),
      ctx
    );
    expect(resp.status).toBe(200);
  });
});

// ─── duplicate-purchase gate ─────────────────────────────────────────
//
// Same email_hash already has an active *paid* license. Three outcomes,
// gated in handleCheckoutCompleted:
//   - existing=lifetime           → BLOCK
//   - existing=mo|an, new=lifetime → IN-PLACE UPGRADE (lifetime "wins")
//   - existing=mo|an, new=mo|an    → BLOCK (duplicate sub)
// Existing=trial always falls through (legitimate trial→paid flow).

describe("/api/stripe/webhook — duplicate-purchase gate", () => {
  let fetchMock: ReturnType<typeof vi.fn>;

  beforeEach(() => {
    fetchMock = vi.fn().mockResolvedValue(
      new Response("{}", { status: 200, headers: { "Content-Type": "application/json" } })
    );
    vi.stubGlobal("fetch", fetchMock);
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  /// Helper — pre-populate state with one active paid license for `email`.
  async function seedLicense(
    state: MockState,
    args: {
      email: string;
      tier: "monthly" | "annual" | "lifetime";
      stripe_subscription_id?: string | null;
    }
  ): Promise<{ licenseId: string; eh: string }> {
    const eh = await emailHash(args.email);
    const licenseId = "lic_existing_" + args.tier;
    state.licenses.set(licenseId, {
      license_id: licenseId,
      email_hash: eh,
      tier: args.tier,
      issued_at: 1000,
      valid_until: 1000 + 366 * 86400,
      max_devices: 5,
      status: "active",
      stripe_session_id: "cs_old_" + args.tier,
      stripe_customer_id: "cus_old",
      stripe_subscription_id:
        args.stripe_subscription_id !== undefined
          ? args.stripe_subscription_id
          : args.tier === "lifetime"
            ? null
            : "sub_old_" + args.tier,
      current_period_end: 1000 + 366 * 86400,
      cancel_at_period_end: 0,
    });
    return { licenseId, eh };
  }

  function checkoutBody(args: {
    eventId: string;
    sessionId: string;
    email: string;
    tier: "monthly" | "annual" | "lifetime";
    customer?: string;
    subscription?: string | null;
  }): string {
    return JSON.stringify({
      id: args.eventId,
      type: "checkout.session.completed",
      data: {
        object: {
          id: args.sessionId,
          mode: args.tier === "lifetime" ? "payment" : "subscription",
          customer: args.customer ?? "cus_new",
          subscription:
            args.subscription !== undefined
              ? args.subscription
              : args.tier === "lifetime"
                ? null
                : "sub_new",
          customer_details: { email: args.email },
          metadata: { tier: args.tier },
        },
      },
    });
  }

  test("monthly + monthly (same email) → blocks duplicate, no second license, magic link issued, new sub cancelled via Stripe API", async () => {
    const state = emptyState();
    const { eh } = await seedLicense(state, { email: "dup@example.com", tier: "monthly" });

    const body = checkoutBody({
      eventId: "evt_dup_mo_mo",
      sessionId: "cs_new_mo",
      email: "dup@example.com",
      tier: "monthly",
      subscription: "sub_brand_new",
    });
    const env = makeEnv(state, { stripeSecret: "sk_test_xxx" });
    const resp = await handleStripeWebhook(await signedRequest(body), env, ctx);
    expect(resp.status).toBe(200);

    // No second license created.
    expect(state.licenses.size).toBe(1);

    // Magic link issued for the EXISTING license.
    expect(state.activation_codes.size).toBe(1);
    const code = [...state.activation_codes.values()][0];
    expect(code.license_id).toBe("lic_existing_monthly");

    // Stripe DELETE called for the new subscription.
    expect(fetchMock).toHaveBeenCalledTimes(1);
    const [url, init] = fetchMock.mock.calls[0];
    expect(url).toBe("https://api.stripe.com/v1/subscriptions/sub_brand_new");
    expect((init as RequestInit).method).toBe("DELETE");
    expect(((init as RequestInit).headers as Record<string, string>).Authorization).toBe(
      "Bearer sk_test_xxx"
    );

    // Audit row recorded.
    const audit = state.audit_log.find(
      (a) => a.event_type === "duplicate_purchase_blocked"
    );
    expect(audit).toBeDefined();
    expect(audit?.email_hash).toBe(eh);
  });

  test("annual + lifetime (same email) → in-place upgrade to lifetime, old sub cancelled, license_id preserved", async () => {
    const state = emptyState();
    const { licenseId } = await seedLicense(state, {
      email: "upgrade@example.com",
      tier: "annual",
      stripe_subscription_id: "sub_old_annual_xyz",
    });

    const body = checkoutBody({
      eventId: "evt_an_to_lt",
      sessionId: "cs_lt_new",
      email: "upgrade@example.com",
      tier: "lifetime",
      subscription: null,
    });
    const env = makeEnv(state, { stripeSecret: "sk_test_xxx" });
    const resp = await handleStripeWebhook(await signedRequest(body), env, ctx);
    expect(resp.status).toBe(200);

    // Same single license row, mutated in place.
    expect(state.licenses.size).toBe(1);
    const lic = state.licenses.get(licenseId)!;
    expect(lic.tier).toBe("lifetime");
    expect(lic.stripe_subscription_id).toBeNull();
    expect(lic.stripe_session_id).toBe("cs_lt_new");
    // Validity window jumped to ≈ 1095 days (lifetime horizon).
    const validFor =
      (lic.valid_until as number) - Math.floor(Date.now() / 1000);
    expect(validFor).toBeGreaterThan(1090 * 86400);

    // Old recurring subscription was cancelled via Stripe API.
    expect(fetchMock).toHaveBeenCalledTimes(1);
    const [url] = fetchMock.mock.calls[0];
    expect(url).toBe("https://api.stripe.com/v1/subscriptions/sub_old_annual_xyz");

    // Magic link minted for the (upgraded) license so the client can
    // pick up a fresh token signed with tier=lifetime.
    expect(state.activation_codes.size).toBe(1);
    const code = [...state.activation_codes.values()][0];
    expect(code.license_id).toBe(licenseId);

    // Audit row recorded with the upgrade event type.
    const audit = state.audit_log.find(
      (a) => a.event_type === "license_upgraded_to_lifetime"
    );
    expect(audit).toBeDefined();
  });

  test("lifetime + monthly (same email) → blocks, no upgrade, no new license, sub cancelled", async () => {
    const state = emptyState();
    await seedLicense(state, { email: "lt@example.com", tier: "lifetime" });

    const body = checkoutBody({
      eventId: "evt_lt_blocks_mo",
      sessionId: "cs_attempt",
      email: "lt@example.com",
      tier: "monthly",
      subscription: "sub_attempted",
    });
    const env = makeEnv(state, { stripeSecret: "sk_test_xxx" });
    const resp = await handleStripeWebhook(await signedRequest(body), env, ctx);
    expect(resp.status).toBe(200);

    // Lifetime license stays exactly as it was.
    expect(state.licenses.size).toBe(1);
    const lic = state.licenses.get("lic_existing_lifetime")!;
    expect(lic.tier).toBe("lifetime");

    // Attempted sub cancelled.
    expect(fetchMock).toHaveBeenCalledTimes(1);
    expect(fetchMock.mock.calls[0][0]).toBe(
      "https://api.stripe.com/v1/subscriptions/sub_attempted"
    );

    // Audit row carries the right reason.
    const audit = state.audit_log.find(
      (a) => a.event_type === "duplicate_purchase_blocked"
    );
    expect(audit).toBeDefined();
    const details = JSON.parse(audit!.details as string);
    expect(details.reason).toBe("lifetime_already_active");
  });

  test("trial + annual (same email) → falls through to normal create (legitimate trial→paid upgrade)", async () => {
    const state = emptyState();
    const eh = await emailHash("trialer@example.com");
    state.licenses.set("lic_trial", {
      license_id: "lic_trial",
      email_hash: eh,
      tier: "trial",
      issued_at: 1000,
      valid_until: 1000 + 14 * 86400,
      max_devices: 5,
      status: "active",
      stripe_session_id: null,
      stripe_customer_id: null,
      stripe_subscription_id: null,
      current_period_end: null,
      cancel_at_period_end: 0,
    });

    const body = checkoutBody({
      eventId: "evt_trial_upgrade",
      sessionId: "cs_first_paid",
      email: "trialer@example.com",
      tier: "annual",
      subscription: "sub_first",
    });
    const env = makeEnv(state, { stripeSecret: "sk_test_xxx" });
    const resp = await handleStripeWebhook(await signedRequest(body), env, ctx);
    expect(resp.status).toBe(200);

    // New annual license created — gate did NOT block.
    expect(state.licenses.size).toBe(2);
    const annual = [...state.licenses.values()].find((l) => l.tier === "annual");
    expect(annual).toBeDefined();
    expect(annual?.stripe_subscription_id).toBe("sub_first");

    // No Stripe cancel call (nothing to cancel).
    expect(fetchMock).not.toHaveBeenCalled();

    // No duplicate audit, only license_purchased.
    const dupAudit = state.audit_log.find(
      (a) => a.event_type === "duplicate_purchase_blocked"
    );
    expect(dupAudit).toBeUndefined();
    const purchasedAudit = state.audit_log.find(
      (a) => a.event_type === "license_purchased"
    );
    expect(purchasedAudit).toBeDefined();
  });

  test("refund.created (succeeded, full) → license revoked, audit notes source=refund.created", async () => {
    // Modern refund event. Worker fetches the parent charge from
    // Stripe API to learn full vs partial; mock that fetch.
    const state = emptyState();
    const eh = await emailHash("refunded@example.com");
    state.licenses.set("lic_to_refund", {
      license_id: "lic_to_refund",
      email_hash: eh,
      tier: "lifetime",
      issued_at: 1000,
      valid_until: 1000 + 1095 * 86400,
      max_devices: 5,
      status: "active",
      stripe_session_id: "cs_lt",
      stripe_customer_id: "cus_refund",
      stripe_subscription_id: null,
      current_period_end: null,
      cancel_at_period_end: 0,
    });

    // First (and only) fetch is the charge lookup. Stub a full-refund
    // shape: amount === amount_refunded.
    fetchMock.mockResolvedValueOnce(
      new Response(
        JSON.stringify({
          id: "ch_lt",
          amount: 9900,
          amount_refunded: 9900,
          customer: "cus_refund",
        }),
        { status: 200, headers: { "Content-Type": "application/json" } }
      )
    );

    const body = JSON.stringify({
      id: "evt_refund_full",
      type: "refund.created",
      data: {
        object: {
          id: "re_full",
          charge: "ch_lt",
          customer: "cus_refund",
          status: "succeeded",
          amount: 9900,
        },
      },
    });
    const env = makeEnv(state, { stripeSecret: "sk_test_xxx" });
    const resp = await handleStripeWebhook(await signedRequest(body), env, ctx);
    expect(resp.status).toBe(200);

    // License revoked.
    const lic = state.licenses.get("lic_to_refund")!;
    expect(lic.status).toBe("revoked");

    // Stripe API was called to fetch the charge (one call, the GET).
    expect(fetchMock).toHaveBeenCalledTimes(1);
    const [url, init] = fetchMock.mock.calls[0];
    expect(url).toBe("https://api.stripe.com/v1/charges/ch_lt");
    // No method on a GET RequestInit → undefined is OK.
    expect((init as RequestInit | undefined)?.method ?? "GET").toBe("GET");

    // Audit row carries the modern source marker.
    const audit = state.audit_log.find(
      (a) => a.event_type === "license_revoked_refund"
    );
    expect(audit).toBeDefined();
    const details = JSON.parse(audit!.details as string);
    expect(details.source).toBe("refund.created");
    expect(details.refund_id).toBe("re_full");
  });

  test("refund.created (succeeded, partial) → license stays active, partial-refund audit only", async () => {
    const state = emptyState();
    const eh = await emailHash("partial@example.com");
    state.licenses.set("lic_partial", {
      license_id: "lic_partial",
      email_hash: eh,
      tier: "annual",
      issued_at: 1000,
      valid_until: 1000 + 366 * 86400,
      max_devices: 5,
      status: "active",
      stripe_session_id: "cs_an",
      stripe_customer_id: "cus_partial",
      stripe_subscription_id: "sub_an",
      current_period_end: 1000 + 366 * 86400,
      cancel_at_period_end: 0,
    });
    fetchMock.mockResolvedValueOnce(
      new Response(
        JSON.stringify({
          id: "ch_an",
          amount: 3900,
          amount_refunded: 1000, // partial
          customer: "cus_partial",
        }),
        { status: 200, headers: { "Content-Type": "application/json" } }
      )
    );

    const body = JSON.stringify({
      id: "evt_refund_partial",
      type: "refund.created",
      data: {
        object: {
          id: "re_partial",
          charge: "ch_an",
          customer: "cus_partial",
          status: "succeeded",
          amount: 1000,
        },
      },
    });
    const env = makeEnv(state, { stripeSecret: "sk_test_xxx" });
    const resp = await handleStripeWebhook(await signedRequest(body), env, ctx);
    expect(resp.status).toBe(200);

    // License unchanged.
    expect(state.licenses.get("lic_partial")!.status).toBe("active");

    // Partial-refund audit recorded.
    const audit = state.audit_log.find(
      (a) => a.event_type === "license_partial_refund"
    );
    expect(audit).toBeDefined();
    const details = JSON.parse(audit!.details as string);
    expect(details.source).toBe("refund.created");
    expect(details.this_refund_amount).toBe(1000);
  });

  test("refund.created status=pending → no fetch, no DB mutation", async () => {
    const state = emptyState();
    state.licenses.set("lic_pending", {
      license_id: "lic_pending",
      email_hash: "irrelevant",
      tier: "monthly",
      issued_at: 1000,
      valid_until: 1000 + 31 * 86400,
      max_devices: 5,
      status: "active",
      stripe_session_id: "cs",
      stripe_customer_id: "cus_pending",
      stripe_subscription_id: "sub_p",
      current_period_end: 1000 + 31 * 86400,
      cancel_at_period_end: 0,
    });

    const body = JSON.stringify({
      id: "evt_refund_pending",
      type: "refund.created",
      data: {
        object: {
          id: "re_pending",
          charge: "ch_p",
          customer: "cus_pending",
          status: "pending",
          amount: 500,
        },
      },
    });
    const env = makeEnv(state, { stripeSecret: "sk_test_xxx" });
    const resp = await handleStripeWebhook(await signedRequest(body), env, ctx);
    expect(resp.status).toBe(200);

    // No charge fetch (status filter dropped it before).
    expect(fetchMock).not.toHaveBeenCalled();
    // License unchanged.
    expect(state.licenses.get("lic_pending")!.status).toBe("active");
  });

  test("refund.created without STRIPE_SECRET_KEY → cannot fetch charge, license untouched (defensive)", async () => {
    const state = emptyState();
    state.licenses.set("lic_unfetched", {
      license_id: "lic_unfetched",
      email_hash: "x",
      tier: "lifetime",
      issued_at: 1000,
      valid_until: 1000 + 1095 * 86400,
      max_devices: 5,
      status: "active",
      stripe_session_id: "cs",
      stripe_customer_id: "cus_x",
      stripe_subscription_id: null,
      current_period_end: null,
      cancel_at_period_end: 0,
    });

    const body = JSON.stringify({
      id: "evt_refund_no_secret",
      type: "refund.created",
      data: {
        object: {
          id: "re_no_secret",
          charge: "ch_x",
          customer: "cus_x",
          status: "succeeded",
          amount: 9900,
        },
      },
    });
    // No stripeSecret → fetch is blocked at the helper.
    const env = makeEnv(state);
    const resp = await handleStripeWebhook(await signedRequest(body), env, ctx);
    expect(resp.status).toBe(200);

    // No fetch attempted because STRIPE_SECRET_KEY was empty.
    expect(fetchMock).not.toHaveBeenCalled();
    // License left untouched — better to stay active than to revoke
    // without confirming full vs partial.
    expect(state.licenses.get("lic_unfetched")!.status).toBe("active");
  });

  test("duplicate without STRIPE_SECRET_KEY → still blocks insert + sends magic link, but logs cancel-skipped", async () => {
    // Defensive: if the operator forgot to set the secret, we must not
    // create a duplicate license row. The cancel-sub call is a separate
    // promise; failing it should never silently allow the duplicate to
    // proceed.
    const state = emptyState();
    await seedLicense(state, { email: "nokey@example.com", tier: "monthly" });

    const body = checkoutBody({
      eventId: "evt_no_secret",
      sessionId: "cs_no_secret",
      email: "nokey@example.com",
      tier: "monthly",
      subscription: "sub_uncancellable",
    });
    // No stripeSecret in opts — env.STRIPE_SECRET_KEY = "".
    const env = makeEnv(state);
    const resp = await handleStripeWebhook(await signedRequest(body), env, ctx);
    expect(resp.status).toBe(200);

    // Still only one license — gate held.
    expect(state.licenses.size).toBe(1);
    // Magic link was still issued.
    expect(state.activation_codes.size).toBe(1);
    // Audit reflects sub was NOT cancelled.
    const audit = state.audit_log.find(
      (a) => a.event_type === "duplicate_purchase_blocked"
    );
    expect(audit).toBeDefined();
    const details = JSON.parse(audit!.details as string);
    expect(details.cancelled_attempted_subscription).toBe(false);
    // No fetch attempted (no secret).
    expect(fetchMock).not.toHaveBeenCalled();
  });
});
