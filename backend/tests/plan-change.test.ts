import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import { handlePlanChange } from "../src/handlers/plan-change";
import { signToken, b64urlEncode, type Claims } from "../src/crypto";
import { emptyState, makeMockDB, type MockState } from "./_d1-mock";
import type { Env } from "../src/index";

// Same keypair pattern as billing-portal.test.ts.
async function makeKeypair(): Promise<{ priv: string; pub: string }> {
  const kp = (await crypto.subtle.generateKey(
    { name: "Ed25519" } as EcKeyGenParams,
    true,
    ["sign", "verify"]
  )) as CryptoKeyPair;
  const pkcs8 = new Uint8Array(
    await crypto.subtle.exportKey("pkcs8", kp.privateKey)
  );
  const seed = pkcs8.slice(16, 48);
  const spki = new Uint8Array(await crypto.subtle.exportKey("spki", kp.publicKey));
  const pub = spki.slice(12, 44);
  return { priv: b64urlEncode(seed), pub: b64urlEncode(pub) };
}

function makeEnv(state: MockState, priv: string, pub: string): Env {
  return {
    DB: makeMockDB(state) as unknown as D1Database,
    PUBLIC_URL: "https://example.test",
    EMAIL_FROM: "Dimmy <hello@dimmy.app>",
    STRIPE_PRICE_MONTHLY: "price_m",
    STRIPE_PRICE_ANNUAL: "price_a",
    STRIPE_PRICE_LIFETIME: "price_l",
    DIMMY_LICENSE_PRIVKEY: priv,
    DIMMY_LICENSE_PUBKEY: pub,
    STRIPE_WEBHOOK_SECRET: "whsec_test",
    STRIPE_SECRET_KEY: "sk_test_fake",
    RESEND_API_KEY: "",
  };
}

async function freshClaims(lid = "01LICENSEABCDEFGHJKMNPQRST"): Promise<Claims> {
  return {
    v: 1,
    lid,
    eh: "abc",
    tier: "monthly",
    iat: 1_700_000_000,
    exp: 1_700_000_000 + 31 * 86400,
    max_offline: 14,
    did: "01DEVICEABCDEFGHJKMNPQRST",
    scope: ["managed_stt"],
  };
}

const ctx = {} as ExecutionContext;

function seedLicense(state: MockState, claims: Claims, overrides: Partial<Record<string, unknown>> = {}) {
  state.licenses.set(claims.lid, {
    license_id: claims.lid,
    email_hash: claims.eh,
    tier: claims.tier,
    issued_at: claims.iat,
    valid_until: claims.exp,
    max_devices: 5,
    status: "active",
    stripe_session_id: "cs_seed",
    stripe_customer_id: "cus_seed",
    stripe_subscription_id: "sub_seed",
    current_period_end: claims.exp,
    cancel_at_period_end: 0,
    ...overrides,
  });
}

describe("/api/plan-change", () => {
  let priv: string, pub: string;
  beforeEach(async () => {
    const kp = await makeKeypair();
    priv = kp.priv;
    pub = kp.pub;
    vi.stubGlobal("fetch", vi.fn());
  });
  afterEach(() => {
    vi.unstubAllGlobals();
  });

  test("400 when token missing", async () => {
    const req = new Request("http://localhost/api/plan-change", {
      method: "POST",
      body: JSON.stringify({ new_tier: "annual" }),
    });
    const resp = await handlePlanChange(req, makeEnv(emptyState(), priv, pub), ctx);
    expect(resp.status).toBe(400);
  });

  test("400 when new_tier is lifetime (must use checkout)", async () => {
    const req = new Request("http://localhost/api/plan-change", {
      method: "POST",
      body: JSON.stringify({ token: "x", new_tier: "lifetime" }),
    });
    const resp = await handlePlanChange(req, makeEnv(emptyState(), priv, pub), ctx);
    expect(resp.status).toBe(400);
    const j = (await resp.json()) as { error: string };
    expect(j.error).toContain("monthly");
    expect(j.error).toContain("annual");
  });

  test("400 when new_tier is something random", async () => {
    const req = new Request("http://localhost/api/plan-change", {
      method: "POST",
      body: JSON.stringify({ token: "x", new_tier: "platinum" }),
    });
    const resp = await handlePlanChange(req, makeEnv(emptyState(), priv, pub), ctx);
    expect(resp.status).toBe(400);
  });

  test("400 when token does not verify", async () => {
    const req = new Request("http://localhost/api/plan-change", {
      method: "POST",
      body: JSON.stringify({ token: "garbage.token.value", new_tier: "annual" }),
    });
    const resp = await handlePlanChange(req, makeEnv(emptyState(), priv, pub), ctx);
    expect(resp.status).toBe(400);
    const j = (await resp.json()) as { error: string };
    expect(j.error).toContain("invalid token");
  });

  test("400 when JSON body is malformed", async () => {
    const req = new Request("http://localhost/api/plan-change", {
      method: "POST",
      body: "{not json",
    });
    const resp = await handlePlanChange(req, makeEnv(emptyState(), priv, pub), ctx);
    expect(resp.status).toBe(400);
  });

  test("404 when license not in DB", async () => {
    const claims = await freshClaims();
    const token = await signToken(claims, priv);
    const req = new Request("http://localhost/api/plan-change", {
      method: "POST",
      body: JSON.stringify({ token, new_tier: "annual" }),
    });
    const resp = await handlePlanChange(req, makeEnv(emptyState(), priv, pub), ctx);
    expect(resp.status).toBe(404);
  });

  test("409 when license is not active", async () => {
    const state = emptyState();
    const claims = await freshClaims();
    seedLicense(state, claims, { status: "revoked" });
    const token = await signToken(claims, priv);
    const req = new Request("http://localhost/api/plan-change", {
      method: "POST",
      body: JSON.stringify({ token, new_tier: "annual" }),
    });
    const resp = await handlePlanChange(req, makeEnv(state, priv, pub), ctx);
    expect(resp.status).toBe(409);
    const j = (await resp.json()) as { error: string };
    expect(j.error).toContain("not active");
  });

  test("200 no_change when current tier == new tier", async () => {
    const state = emptyState();
    const claims = await freshClaims();
    seedLicense(state, claims, { tier: "annual" });
    const token = await signToken(claims, priv);
    const req = new Request("http://localhost/api/plan-change", {
      method: "POST",
      body: JSON.stringify({ token, new_tier: "annual" }),
    });
    const resp = await handlePlanChange(req, makeEnv(state, priv, pub), ctx);
    expect(resp.status).toBe(200);
    const j = (await resp.json()) as { status: string; tier: string };
    expect(j.status).toBe("no_change");
    expect(j.tier).toBe("annual");
    // Stripe should never be called.
    expect((fetch as unknown as ReturnType<typeof vi.fn>).mock.calls.length).toBe(0);
  });

  test("409 when license is on trial tier", async () => {
    const state = emptyState();
    const claims = await freshClaims();
    seedLicense(state, claims, { tier: "trial", stripe_subscription_id: null });
    const token = await signToken(claims, priv);
    const req = new Request("http://localhost/api/plan-change", {
      method: "POST",
      body: JSON.stringify({ token, new_tier: "annual" }),
    });
    const resp = await handlePlanChange(req, makeEnv(state, priv, pub), ctx);
    expect(resp.status).toBe(409);
    const j = (await resp.json()) as { error: string };
    expect(j.error).toContain("trial");
  });

  test("409 when license is on lifetime tier", async () => {
    const state = emptyState();
    const claims = await freshClaims();
    seedLicense(state, claims, { tier: "lifetime", stripe_subscription_id: null });
    const token = await signToken(claims, priv);
    const req = new Request("http://localhost/api/plan-change", {
      method: "POST",
      body: JSON.stringify({ token, new_tier: "annual" }),
    });
    const resp = await handlePlanChange(req, makeEnv(state, priv, pub), ctx);
    expect(resp.status).toBe(409);
    const j = (await resp.json()) as { error: string };
    expect(j.error).toContain("lifetime");
  });

  test("409 when license is missing stripe_subscription_id", async () => {
    const state = emptyState();
    const claims = await freshClaims();
    seedLicense(state, claims, { tier: "monthly", stripe_subscription_id: null });
    const token = await signToken(claims, priv);
    const req = new Request("http://localhost/api/plan-change", {
      method: "POST",
      body: JSON.stringify({ token, new_tier: "annual" }),
    });
    const resp = await handlePlanChange(req, makeEnv(state, priv, pub), ctx);
    expect(resp.status).toBe(409);
    const j = (await resp.json()) as { error: string };
    expect(j.error).toContain("subscription id");
  });

  test("502 when Stripe sub fetch fails", async () => {
    const state = emptyState();
    const claims = await freshClaims();
    seedLicense(state, claims, { tier: "monthly", stripe_subscription_id: "sub_dead" });
    const token = await signToken(claims, priv);
    (fetch as unknown as ReturnType<typeof vi.fn>).mockResolvedValueOnce(
      new Response("No such subscription: sub_dead", { status: 404 })
    );
    const req = new Request("http://localhost/api/plan-change", {
      method: "POST",
      body: JSON.stringify({ token, new_tier: "annual" }),
    });
    const resp = await handlePlanChange(req, makeEnv(state, priv, pub), ctx);
    expect(resp.status).toBe(502);
    const j = (await resp.json()) as { error: string };
    expect(j.error).toContain("stripe sub fetch");
  });

  test("502 when Stripe sub returns no items", async () => {
    const state = emptyState();
    const claims = await freshClaims();
    seedLicense(state, claims, { tier: "monthly", stripe_subscription_id: "sub_empty" });
    const token = await signToken(claims, priv);
    (fetch as unknown as ReturnType<typeof vi.fn>).mockResolvedValueOnce(
      new Response(JSON.stringify({ items: { data: [] } }), { status: 200 })
    );
    const req = new Request("http://localhost/api/plan-change", {
      method: "POST",
      body: JSON.stringify({ token, new_tier: "annual" }),
    });
    const resp = await handlePlanChange(req, makeEnv(state, priv, pub), ctx);
    expect(resp.status).toBe(502);
    const j = (await resp.json()) as { error: string };
    expect(j.error).toContain("items[0]");
  });

  test("502 when Stripe update call fails", async () => {
    const state = emptyState();
    const claims = await freshClaims();
    seedLicense(state, claims, { tier: "monthly", stripe_subscription_id: "sub_upd_err" });
    const token = await signToken(claims, priv);
    const fetchMock = fetch as unknown as ReturnType<typeof vi.fn>;
    fetchMock.mockResolvedValueOnce(
      new Response(JSON.stringify({ items: { data: [{ id: "si_x" }] } }), { status: 200 })
    );
    fetchMock.mockResolvedValueOnce(
      new Response("price not found", { status: 400 })
    );
    const req = new Request("http://localhost/api/plan-change", {
      method: "POST",
      body: JSON.stringify({ token, new_tier: "annual" }),
    });
    const resp = await handlePlanChange(req, makeEnv(state, priv, pub), ctx);
    expect(resp.status).toBe(502);
    const j = (await resp.json()) as { error: string };
    expect(j.error).toContain("stripe sub update");
  });

  test("200 happy path monthly → annual: posts proration update + writes audit row", async () => {
    const state = emptyState();
    const claims = await freshClaims();
    seedLicense(state, claims, { tier: "monthly", stripe_subscription_id: "sub_happy" });
    const token = await signToken(claims, priv);
    const fetchMock = fetch as unknown as ReturnType<typeof vi.fn>;
    fetchMock.mockResolvedValueOnce(
      new Response(JSON.stringify({ items: { data: [{ id: "si_42" }] } }), { status: 200 })
    );
    fetchMock.mockResolvedValueOnce(
      new Response(JSON.stringify({ id: "sub_happy", items: { data: [{ id: "si_42", price: { id: "price_a" } }] } }), { status: 200 })
    );
    const req = new Request("http://localhost/api/plan-change", {
      method: "POST",
      body: JSON.stringify({ token, new_tier: "annual" }),
    });
    const resp = await handlePlanChange(req, makeEnv(state, priv, pub), ctx);
    expect(resp.status).toBe(200);
    const j = (await resp.json()) as { status: string; new_tier: string };
    expect(j.status).toBe("plan_changed");
    expect(j.new_tier).toBe("annual");

    // Two Stripe calls: one GET for items, one POST for the update.
    expect(fetchMock).toHaveBeenCalledTimes(2);
    const [getUrl, getInit] = fetchMock.mock.calls[0];
    expect(getUrl).toBe("https://api.stripe.com/v1/subscriptions/sub_happy");
    expect((getInit as RequestInit | undefined)?.method ?? "GET").toBe("GET");
    const [postUrl, postInit] = fetchMock.mock.calls[1];
    expect(postUrl).toBe("https://api.stripe.com/v1/subscriptions/sub_happy");
    expect((postInit as RequestInit).method).toBe("POST");
    const body = (postInit as RequestInit).body as string;
    expect(body).toContain("items%5B0%5D%5Bid%5D=si_42");
    expect(body).toContain("items%5B0%5D%5Bprice%5D=price_a");
    expect(body).toContain("proration_behavior=create_prorations");
    expect(body).toContain("metadata%5Btier%5D=annual");

    // Audit row written.
    const audits = state.audit_log.filter((r) => r.event_type === "plan_changed");
    expect(audits).toHaveLength(1);
    const details = JSON.parse(audits[0].details as string) as {
      previous_tier: string;
      new_tier: string;
      stripe_subscription_id: string;
      stripe_item_id: string;
    };
    expect(details.previous_tier).toBe("monthly");
    expect(details.new_tier).toBe("annual");
    expect(details.stripe_subscription_id).toBe("sub_happy");
    expect(details.stripe_item_id).toBe("si_42");
  });

  test("500 when STRIPE_SECRET_KEY missing", async () => {
    const env = makeEnv(emptyState(), priv, pub);
    env.STRIPE_SECRET_KEY = "";
    const req = new Request("http://localhost/api/plan-change", {
      method: "POST",
      body: JSON.stringify({ token: "x", new_tier: "annual" }),
    });
    const resp = await handlePlanChange(req, env, ctx);
    expect(resp.status).toBe(500);
  });
});
