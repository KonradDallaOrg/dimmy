// End-to-end tests of /api/stripe/webhook against an in-memory D1 mock.
// Exercises the production stripe handler — same code path Stripe will
// hit in production, with real HMAC sigs the handler verifies.

import { describe, expect, test } from "vitest";
import { handleStripeWebhook } from "../src/handlers/stripe";
import { emptyState, makeMockDB, type MockState } from "./_d1-mock";
import type { Env } from "../src/index";

const SECRET = "whsec_test_secret";

function makeEnv(state: MockState): Env {
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
});
