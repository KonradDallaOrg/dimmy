// /api/checkout/create — verifies the request → Stripe Checkout Session
// shape. We stub global.fetch to capture the form-encoded body the
// handler POSTs to api.stripe.com so the assertions are made against
// what would actually hit Stripe in prod.

import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import { handleCheckoutCreate } from "../src/handlers/checkout";
import { signToken } from "../src/crypto";
import { emptyState, makeMockDB } from "./_d1-mock";
import type { Env } from "../src/index";

const PRIV = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
const PUB  = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";

function makeEnv(overrides: Partial<Env> = {}, state = emptyState()): Env {
  return {
    DB: makeMockDB(state) as unknown as D1Database,
    PUBLIC_URL: "http://localhost:8787",
    EMAIL_FROM: "Dimmy <hello@dimmy.app>",
    STRIPE_PRICE_MONTHLY: "price_monthly_test",
    STRIPE_PRICE_ANNUAL:  "price_annual_test",
    STRIPE_PRICE_LIFETIME:"price_lifetime_test",
    DIMMY_LICENSE_PRIVKEY: PRIV,
    DIMMY_LICENSE_PUBKEY:  PUB,
    STRIPE_WEBHOOK_SECRET: "whsec_test_secret",
    STRIPE_SECRET_KEY: "sk_test_dummy",
    RESEND_API_KEY: "",
    ...overrides,
  };
}

// Generate a real Ed25519 keypair so signToken/verifyTokenWithPub
// round-trip in the gate tests below.
async function realKeypair(): Promise<{ priv: string; pub: string }> {
  const kp = (await crypto.subtle.generateKey(
    { name: "Ed25519" } as EcKeyGenParams, true, ["sign", "verify"]
  )) as CryptoKeyPair;
  const pkcs8 = new Uint8Array(await crypto.subtle.exportKey("pkcs8", kp.privateKey));
  const seed = pkcs8.slice(16, 48);
  const spki = new Uint8Array(await crypto.subtle.exportKey("spki", kp.publicKey));
  const pubRaw = spki.slice(12, 44);
  const b64u = (b: Uint8Array) => Buffer.from(b).toString("base64url");
  return { priv: b64u(seed), pub: b64u(pubRaw) };
}

const ctx = {} as ExecutionContext;

let lastFetch: { url: string; init?: RequestInit } | null = null;

beforeEach(() => {
  lastFetch = null;
  globalThis.fetch = vi.fn(async (input: any, init?: RequestInit) => {
    const url = typeof input === "string" ? input : input.url;
    lastFetch = { url, init };
    return new Response(
      JSON.stringify({
        id: "cs_test_fake_session",
        url: "https://checkout.stripe.com/c/pay/cs_test_fake",
      }),
      { status: 200, headers: { "Content-Type": "application/json" } }
    );
  }) as typeof fetch;
});

afterEach(() => {
  vi.restoreAllMocks();
});

function makeReq(body: unknown): Request {
  return new Request("http://localhost/api/checkout/create", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body),
  });
}

function parseFormBody(init: RequestInit | undefined): URLSearchParams {
  return new URLSearchParams(String(init?.body ?? ""));
}

describe("/api/checkout/create", () => {
  test("400 when tier is missing", async () => {
    const resp = await handleCheckoutCreate(makeReq({}), makeEnv(), ctx);
    expect(resp.status).toBe(400);
    expect(await resp.json()).toEqual({ error: expect.stringMatching(/tier/) });
  });

  test("400 when tier is not one of monthly/annual/lifetime", async () => {
    const resp = await handleCheckoutCreate(
      makeReq({ tier: "platinum" }),
      makeEnv(),
      ctx
    );
    expect(resp.status).toBe(400);
  });

  test("400 when tier is 'trial' (paid endpoint only)", async () => {
    const resp = await handleCheckoutCreate(
      makeReq({ tier: "trial" }),
      makeEnv(),
      ctx
    );
    expect(resp.status).toBe(400);
  });

  test("500 when STRIPE_SECRET_KEY missing — fails closed", async () => {
    const resp = await handleCheckoutCreate(
      makeReq({ tier: "monthly" }),
      makeEnv({ STRIPE_SECRET_KEY: "" }),
      ctx
    );
    expect(resp.status).toBe(500);
  });

  test("500 when price ID for tier missing", async () => {
    const resp = await handleCheckoutCreate(
      makeReq({ tier: "monthly" }),
      makeEnv({ STRIPE_PRICE_MONTHLY: "" }),
      ctx
    );
    expect(resp.status).toBe(500);
  });

  test("monthly → mode=subscription + price_id correctly forwarded", async () => {
    const resp = await handleCheckoutCreate(
      makeReq({ tier: "monthly" }),
      makeEnv(),
      ctx
    );
    expect(resp.status).toBe(200);
    const params = parseFormBody(lastFetch!.init);
    expect(params.get("mode")).toBe("subscription");
    expect(params.get("line_items[0][price]")).toBe("price_monthly_test");
    expect(params.get("line_items[0][quantity]")).toBe("1");
    expect(params.get("metadata[tier]")).toBe("monthly");
  });

  test("annual → mode=subscription + price_annual_test", async () => {
    const resp = await handleCheckoutCreate(
      makeReq({ tier: "annual" }),
      makeEnv(),
      ctx
    );
    expect(resp.status).toBe(200);
    const params = parseFormBody(lastFetch!.init);
    expect(params.get("mode")).toBe("subscription");
    expect(params.get("line_items[0][price]")).toBe("price_annual_test");
    expect(params.get("metadata[tier]")).toBe("annual");
  });

  test("lifetime → mode=payment (one-time, NOT subscription)", async () => {
    const resp = await handleCheckoutCreate(
      makeReq({ tier: "lifetime" }),
      makeEnv(),
      ctx
    );
    expect(resp.status).toBe(200);
    const params = parseFormBody(lastFetch!.init);
    expect(params.get("mode")).toBe("payment");
    expect(params.get("line_items[0][price]")).toBe("price_lifetime_test");
    expect(params.get("metadata[tier]")).toBe("lifetime");
  });

  test("billing address is required (so Stripe Tax can compute)", async () => {
    await handleCheckoutCreate(makeReq({ tier: "annual" }), makeEnv(), ctx);
    const params = parseFormBody(lastFetch!.init);
    expect(params.get("billing_address_collection")).toBe("required");
  });

  test("token (trial-upgrade) → carries email_hash via client_reference_id", async () => {
    // Real Ed25519 keypair so signToken/verifyTokenWithPub round-trip.
    const kp = (await crypto.subtle.generateKey(
      { name: "Ed25519" } as EcKeyGenParams,
      true,
      ["sign", "verify"]
    )) as CryptoKeyPair;
    const pkcs8 = new Uint8Array(await crypto.subtle.exportKey("pkcs8", kp.privateKey));
    const seed = pkcs8.slice(16, 48);
    const spki = new Uint8Array(await crypto.subtle.exportKey("spki", kp.publicKey));
    const pubRaw = spki.slice(12, 44);
    const b64u = (b: Uint8Array) =>
      Buffer.from(b).toString("base64url");
    const realPriv = b64u(seed);
    const realPub  = b64u(pubRaw);

    const env = makeEnv({ DIMMY_LICENSE_PRIVKEY: realPriv, DIMMY_LICENSE_PUBKEY: realPub });
    const claims = {
      v: 1,
      lid: "01ABC",
      eh: "abc123emailhash",
      tier: "trial",
      iat: 1000,
      exp: 2000,
      max_offline: 30,
      did: "01DID",
      scope: ["managed_stt"],
    };
    const token = await signToken(claims as any, realPriv);
    const resp = await handleCheckoutCreate(
      makeReq({ tier: "annual", token }),
      env,
      ctx
    );
    expect(resp.status).toBe(200);
    const params = parseFormBody(lastFetch!.init);
    expect(params.get("client_reference_id")).toBe("abc123emailhash");
  });

  test("invalid token → silent fall-through (anonymous purchase)", async () => {
    const resp = await handleCheckoutCreate(
      makeReq({ tier: "annual", token: "garbage.tampered.signature" }),
      makeEnv(),
      ctx
    );
    expect(resp.status).toBe(200);
    const params = parseFormBody(lastFetch!.init);
    expect(params.get("client_reference_id")).toBeNull();
  });

  test("https return_url is honoured", async () => {
    await handleCheckoutCreate(
      makeReq({ tier: "annual", return_url: "https://my.app/back" }),
      makeEnv(),
      ctx
    );
    const params = parseFormBody(lastFetch!.init);
    expect(params.get("success_url")).toBe("https://my.app/back");
    expect(params.get("cancel_url")).toBe("https://my.app/back");
  });

  test("dimmy:// return_url is honoured", async () => {
    await handleCheckoutCreate(
      makeReq({ tier: "annual", return_url: "dimmy://license" }),
      makeEnv(),
      ctx
    );
    const params = parseFormBody(lastFetch!.init);
    expect(params.get("success_url")).toBe("dimmy://license");
  });

  test("javascript: return_url is rejected (falls back to PUBLIC_URL)", async () => {
    await handleCheckoutCreate(
      makeReq({ tier: "annual", return_url: "javascript:alert(1)" }),
      makeEnv(),
      ctx
    );
    const params = parseFormBody(lastFetch!.init);
    expect(params.get("success_url")).toMatch(/^http:\/\/localhost:8787\//);
  });

  test("oversized return_url is rejected", async () => {
    const huge = "https://" + "a".repeat(3000);
    await handleCheckoutCreate(
      makeReq({ tier: "annual", return_url: huge }),
      makeEnv(),
      ctx
    );
    const params = parseFormBody(lastFetch!.init);
    expect(params.get("success_url")).not.toBe(huge);
  });

  test("502 when Stripe API errors", async () => {
    globalThis.fetch = vi.fn(async () =>
      new Response("Stripe internal", { status: 503 })
    ) as typeof fetch;
    const resp = await handleCheckoutCreate(
      makeReq({ tier: "annual" }),
      makeEnv(),
      ctx
    );
    expect(resp.status).toBe(502);
  });

  test("502 when Stripe response has no url", async () => {
    globalThis.fetch = vi.fn(async () =>
      new Response(JSON.stringify({ id: "cs_test", /* no url */ }), {
        status: 200,
        headers: { "Content-Type": "application/json" },
      })
    ) as typeof fetch;
    const resp = await handleCheckoutCreate(
      makeReq({ tier: "annual" }),
      makeEnv(),
      ctx
    );
    expect(resp.status).toBe(502);
  });

  test("response shape: {url, tier}", async () => {
    const resp = await handleCheckoutCreate(
      makeReq({ tier: "annual" }),
      makeEnv(),
      ctx
    );
    expect(resp.status).toBe(200);
    const body = await resp.json();
    expect(body).toEqual({
      url: "https://checkout.stripe.com/c/pay/cs_test_fake",
      tier: "annual",
    });
  });

  test("invalid JSON body → 400", async () => {
    const req = new Request("http://localhost/api/checkout/create", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: "{not json",
    });
    const resp = await handleCheckoutCreate(req, makeEnv(), ctx);
    expect(resp.status).toBe(400);
  });

  // ── Pre-checkout duplicate gate ──────────────────────────────────────
  // These verify the reject-up-front behaviour: the handler refuses to
  // even hit Stripe when the user already has an active paid license
  // that would either duplicate-charge or should go through plan-change.
  describe("pre-checkout duplicate gate (token + active license)", () => {
    test("active monthly + buy annual → 409 'use plan-change' (no Stripe call)", async () => {
      const { priv, pub } = await realKeypair();
      const state = emptyState();
      state.licenses.set("01LID_MO", {
        license_id: "01LID_MO", email_hash: "eh1", tier: "monthly",
        issued_at: 1, valid_until: 9_999_999_999, max_devices: 5,
        status: "active", stripe_session_id: "cs", stripe_customer_id: "cus",
        stripe_subscription_id: "sub", current_period_end: 9_999_999_999,
        cancel_at_period_end: 0,
      });
      const env = makeEnv(
        { DIMMY_LICENSE_PRIVKEY: priv, DIMMY_LICENSE_PUBKEY: pub }, state);
      const token = await signToken({
        v: 1, lid: "01LID_MO", eh: "eh1", tier: "monthly",
        iat: 1, exp: 9_999_999_999, max_offline: 30, did: "01D",
        scope: ["managed_stt"],
      } as any, priv);
      const resp = await handleCheckoutCreate(
        makeReq({ tier: "annual", token }), env, ctx);
      expect(resp.status).toBe(409);
      const j = (await resp.json()) as { error: string; current_tier: string };
      expect(j.error).toMatch(/plan-change/);
      expect(j.current_tier).toBe("monthly");
      // No Stripe call — gate ran before fetch.
      expect(lastFetch).toBeNull();
    });

    test("active annual + buy monthly → 409 (no Stripe call)", async () => {
      const { priv, pub } = await realKeypair();
      const state = emptyState();
      state.licenses.set("01LID_AN", {
        license_id: "01LID_AN", email_hash: "eh2", tier: "annual",
        issued_at: 1, valid_until: 9_999_999_999, max_devices: 5,
        status: "active", stripe_session_id: "cs", stripe_customer_id: "cus",
        stripe_subscription_id: "sub", current_period_end: 9_999_999_999,
        cancel_at_period_end: 0,
      });
      const env = makeEnv(
        { DIMMY_LICENSE_PRIVKEY: priv, DIMMY_LICENSE_PUBKEY: pub }, state);
      const token = await signToken({
        v: 1, lid: "01LID_AN", eh: "eh2", tier: "annual",
        iat: 1, exp: 9_999_999_999, max_offline: 30, did: "01D",
        scope: ["managed_stt"],
      } as any, priv);
      const resp = await handleCheckoutCreate(
        makeReq({ tier: "monthly", token }), env, ctx);
      expect(resp.status).toBe(409);
      expect(lastFetch).toBeNull();
    });

    test("active lifetime + buy ANY → 409 (lifetime is the ceiling)", async () => {
      const { priv, pub } = await realKeypair();
      const state = emptyState();
      state.licenses.set("01LID_LT", {
        license_id: "01LID_LT", email_hash: "eh3", tier: "lifetime",
        issued_at: 1, valid_until: 9_999_999_999, max_devices: 5,
        status: "active", stripe_session_id: "cs", stripe_customer_id: "cus",
        stripe_subscription_id: null, current_period_end: null,
        cancel_at_period_end: 0,
      });
      const env = makeEnv(
        { DIMMY_LICENSE_PRIVKEY: priv, DIMMY_LICENSE_PUBKEY: pub }, state);
      const token = await signToken({
        v: 1, lid: "01LID_LT", eh: "eh3", tier: "lifetime",
        iat: 1, exp: 9_999_999_999, max_offline: 30, did: "01D",
        scope: ["managed_stt"],
      } as any, priv);
      for (const buyTier of ["monthly", "annual", "lifetime"] as const) {
        const resp = await handleCheckoutCreate(
          makeReq({ tier: buyTier, token }), env, ctx);
        expect(resp.status).toBe(409);
        expect(lastFetch).toBeNull();
      }
    });

    test("active monthly + buy lifetime → PASS (legitimate sub→lifetime upgrade)", async () => {
      const { priv, pub } = await realKeypair();
      const state = emptyState();
      state.licenses.set("01LID_UP", {
        license_id: "01LID_UP", email_hash: "ehUp", tier: "monthly",
        issued_at: 1, valid_until: 9_999_999_999, max_devices: 5,
        status: "active", stripe_session_id: "cs", stripe_customer_id: "cus",
        stripe_subscription_id: "sub", current_period_end: 9_999_999_999,
        cancel_at_period_end: 0,
      });
      const env = makeEnv(
        { DIMMY_LICENSE_PRIVKEY: priv, DIMMY_LICENSE_PUBKEY: pub }, state);
      const token = await signToken({
        v: 1, lid: "01LID_UP", eh: "ehUp", tier: "monthly",
        iat: 1, exp: 9_999_999_999, max_offline: 30, did: "01D",
        scope: ["managed_stt"],
      } as any, priv);
      const resp = await handleCheckoutCreate(
        makeReq({ tier: "lifetime", token }), env, ctx);
      expect(resp.status).toBe(200);
      // Stripe IS called for the lifetime upgrade path.
      expect(lastFetch).not.toBeNull();
      const params = parseFormBody(lastFetch!.init);
      expect(params.get("metadata[tier]")).toBe("lifetime");
    });

    test("active trial + buy annual → PASS (trial→paid upgrade) + carries email_hash", async () => {
      const { priv, pub } = await realKeypair();
      const state = emptyState();
      state.licenses.set("01LID_TR", {
        license_id: "01LID_TR", email_hash: "ehTr", tier: "trial",
        issued_at: 1, valid_until: 9_999_999_999, max_devices: 5,
        status: "active", stripe_session_id: null, stripe_customer_id: null,
        stripe_subscription_id: null, current_period_end: null,
        cancel_at_period_end: 0,
      });
      const env = makeEnv(
        { DIMMY_LICENSE_PRIVKEY: priv, DIMMY_LICENSE_PUBKEY: pub }, state);
      const token = await signToken({
        v: 1, lid: "01LID_TR", eh: "ehTr", tier: "trial",
        iat: 1, exp: 9_999_999_999, max_offline: 30, did: "01D",
        scope: ["managed_stt"],
      } as any, priv);
      const resp = await handleCheckoutCreate(
        makeReq({ tier: "annual", token }), env, ctx);
      expect(resp.status).toBe(200);
      const params = parseFormBody(lastFetch!.init);
      expect(params.get("client_reference_id")).toBe("ehTr");
      expect(params.get("metadata[tier]")).toBe("annual");
    });

    test("revoked license in DB + buy annual → PASS (fresh purchase)", async () => {
      const { priv, pub } = await realKeypair();
      const state = emptyState();
      state.licenses.set("01LID_RV", {
        license_id: "01LID_RV", email_hash: "ehRv", tier: "annual",
        issued_at: 1, valid_until: 9_999_999_999, max_devices: 5,
        status: "revoked", stripe_session_id: "cs", stripe_customer_id: "cus",
        stripe_subscription_id: "sub", current_period_end: null,
        cancel_at_period_end: 0,
      });
      const env = makeEnv(
        { DIMMY_LICENSE_PRIVKEY: priv, DIMMY_LICENSE_PUBKEY: pub }, state);
      const token = await signToken({
        v: 1, lid: "01LID_RV", eh: "ehRv", tier: "annual",
        iat: 1, exp: 9_999_999_999, max_offline: 30, did: "01D",
        scope: ["managed_stt"],
      } as any, priv);
      const resp = await handleCheckoutCreate(
        makeReq({ tier: "annual", token }), env, ctx);
      expect(resp.status).toBe(200);
    });
  });

  // ── Email-based pre-checkout gate (post-sign-out flow) ─────────────
  // The user has a paid license in DB but their device-side license.json
  // is gone (Sign out / clear). The client UI prompts for email before
  // hitting Checkout; the server uses email_hash to find the license
  // and 409s the request, telling the UI to fall back to the activate
  // flow instead. Same matrix as the token path, just keyed by email.
  describe("pre-checkout email gate (post-sign-out / first-purchase)", () => {
    function seedLicenseRow(state: ReturnType<typeof emptyState>, email: string, tier: "monthly" | "annual" | "lifetime" | "trial", overrides: Partial<Record<string, unknown>> = {}) {
      const eh = require("crypto").createHash("sha256").update(email.trim().toLowerCase()).digest("hex");
      const lid = "01LID_" + tier.toUpperCase();
      state.licenses.set(lid, {
        license_id: lid,
        email_hash: eh,
        tier,
        issued_at: 1,
        valid_until: 9_999_999_999,
        max_devices: 5,
        status: "active",
        stripe_session_id: "cs",
        stripe_customer_id: "cus",
        stripe_subscription_id: tier === "lifetime" ? null : "sub",
        current_period_end: 9_999_999_999,
        cancel_at_period_end: 0,
        ...overrides,
      });
      return { lid, eh };
    }

    test("email belongs to active monthly + buy annual → 409 (no Stripe)", async () => {
      const state = emptyState();
      seedLicenseRow(state, "konrad@example.com", "monthly");
      const env = makeEnv({}, state);
      const resp = await handleCheckoutCreate(
        makeReq({ tier: "annual", email: "konrad@example.com" }), env, ctx);
      expect(resp.status).toBe(409);
      expect(lastFetch).toBeNull();
      const j = (await resp.json()) as { error: string; current_tier: string };
      expect(j.current_tier).toBe("monthly");
    });

    test("email belongs to active lifetime + buy * → 409", async () => {
      const state = emptyState();
      seedLicenseRow(state, "ltime@example.com", "lifetime");
      const env = makeEnv({}, state);
      for (const tier of ["monthly", "annual", "lifetime"] as const) {
        lastFetch = null;
        const resp = await handleCheckoutCreate(
          makeReq({ tier, email: "ltime@example.com" }), env, ctx);
        expect(resp.status).toBe(409);
        expect(lastFetch).toBeNull();
      }
    });

    test("email belongs to active monthly + buy lifetime → PASS (sub→lifetime upgrade)", async () => {
      const state = emptyState();
      seedLicenseRow(state, "upgrade@example.com", "monthly");
      const env = makeEnv({}, state);
      const resp = await handleCheckoutCreate(
        makeReq({ tier: "lifetime", email: "upgrade@example.com" }), env, ctx);
      expect(resp.status).toBe(200);
      // customer_email passed to Stripe → dedup the customer object.
      const params = parseFormBody(lastFetch!.init);
      expect(params.get("customer_email")).toBe("upgrade@example.com");
    });

    test("unknown email + any buy → PASS, customer_email forwarded to Stripe", async () => {
      const state = emptyState();
      const env = makeEnv({}, state);
      const resp = await handleCheckoutCreate(
        makeReq({ tier: "monthly", email: "fresh@example.com" }), env, ctx);
      expect(resp.status).toBe(200);
      const params = parseFormBody(lastFetch!.init);
      expect(params.get("customer_email")).toBe("fresh@example.com");
    });

    test("malformed email is silently ignored (gate not applied, no customer_email)", async () => {
      const state = emptyState();
      const env = makeEnv({}, state);
      const resp = await handleCheckoutCreate(
        makeReq({ tier: "monthly", email: "not-an-email" }), env, ctx);
      expect(resp.status).toBe(200);
      const params = parseFormBody(lastFetch!.init);
      expect(params.get("customer_email")).toBeNull();
    });

    test("token wins over email when both provided", async () => {
      const { priv, pub } = await realKeypair();
      const state = emptyState();
      // Token's lid points at a lifetime license (block).
      state.licenses.set("01LID_TOKEN", {
        license_id: "01LID_TOKEN", email_hash: "tokenEh", tier: "lifetime",
        issued_at: 1, valid_until: 9_999_999_999, max_devices: 5,
        status: "active", stripe_session_id: null, stripe_customer_id: null,
        stripe_subscription_id: null, current_period_end: null, cancel_at_period_end: 0,
      });
      // Email points at a different unrelated license that would PASS.
      seedLicenseRow(state, "email@example.com", "trial");
      const env = makeEnv(
        { DIMMY_LICENSE_PRIVKEY: priv, DIMMY_LICENSE_PUBKEY: pub }, state);
      const token = await signToken({
        v: 1, lid: "01LID_TOKEN", eh: "tokenEh", tier: "lifetime",
        iat: 1, exp: 9_999_999_999, max_offline: 30, did: "01D",
        scope: ["managed_stt"],
      } as any, priv);
      const resp = await handleCheckoutCreate(
        makeReq({ tier: "monthly", token, email: "email@example.com" }), env, ctx);
      // Token's lifetime license blocks; email is irrelevant.
      expect(resp.status).toBe(409);
    });
  });
});
