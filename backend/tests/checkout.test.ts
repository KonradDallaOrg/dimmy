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

function makeEnv(overrides: Partial<Env> = {}): Env {
  const state = emptyState();
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
});
