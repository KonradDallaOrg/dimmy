import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import { handleBillingPortal } from "../src/handlers/billing-portal";
import { signToken, b64urlEncode, type Claims } from "../src/crypto";
import { emptyState, makeMockDB, type MockState } from "./_d1-mock";
import type { Env } from "../src/index";

// Generate a fresh keypair per test run — same trick as crypto.test.ts.
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

describe("/api/billing-portal", () => {
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
    const state = emptyState();
    const req = new Request("http://localhost/api/billing-portal", {
      method: "POST",
      body: "{}",
    });
    const resp = await handleBillingPortal(req, makeEnv(state, priv, pub), ctx);
    expect(resp.status).toBe(400);
  });

  test("400 when token invalid", async () => {
    const state = emptyState();
    const req = new Request("http://localhost/api/billing-portal", {
      method: "POST",
      body: JSON.stringify({ token: "garbage.token.value" }),
    });
    const resp = await handleBillingPortal(req, makeEnv(state, priv, pub), ctx);
    expect(resp.status).toBe(400);
  });

  test("404 when license not in DB", async () => {
    const state = emptyState();
    const claims = await freshClaims();
    const token = await signToken(claims, priv);
    const req = new Request("http://localhost/api/billing-portal", {
      method: "POST",
      body: JSON.stringify({ token }),
    });
    const resp = await handleBillingPortal(req, makeEnv(state, priv, pub), ctx);
    expect(resp.status).toBe(404);
  });

  test("409 for trial license (no stripe_customer_id)", async () => {
    const state = emptyState();
    const claims = await freshClaims();
    state.licenses.set(claims.lid, {
      license_id: claims.lid,
      email_hash: claims.eh,
      tier: "trial",
      issued_at: claims.iat,
      valid_until: claims.exp,
      max_devices: 5,
      status: "active",
      stripe_session_id: null,
      stripe_customer_id: null,
      stripe_subscription_id: null,
      current_period_end: null,
      cancel_at_period_end: 0,
    });
    const token = await signToken(claims, priv);
    const req = new Request("http://localhost/api/billing-portal", {
      method: "POST",
      body: JSON.stringify({ token }),
    });
    const resp = await handleBillingPortal(req, makeEnv(state, priv, pub), ctx);
    expect(resp.status).toBe(409);
    const j = (await resp.json()) as { error: string };
    expect(j.error).toContain("Stripe billing");
  });

  test("200 with portal URL when license has stripe_customer_id", async () => {
    const state = emptyState();
    const claims = await freshClaims();
    state.licenses.set(claims.lid, {
      license_id: claims.lid,
      email_hash: claims.eh,
      tier: "monthly",
      issued_at: claims.iat,
      valid_until: claims.exp,
      max_devices: 5,
      status: "active",
      stripe_session_id: "cs_test",
      stripe_customer_id: "cus_test_abc",
      stripe_subscription_id: "sub_test_abc",
      current_period_end: claims.exp,
      cancel_at_period_end: 0,
    });
    const token = await signToken(claims, priv);

    // Mock the Stripe API call.
    const portalUrl =
      "https://billing.stripe.com/p/session/test_FAKEPORTALSESSION";
    (fetch as unknown as ReturnType<typeof vi.fn>).mockResolvedValueOnce(
      new Response(JSON.stringify({ url: portalUrl }), {
        status: 200,
        headers: { "Content-Type": "application/json" },
      })
    );

    const req = new Request("http://localhost/api/billing-portal", {
      method: "POST",
      body: JSON.stringify({ token }),
    });
    const resp = await handleBillingPortal(req, makeEnv(state, priv, pub), ctx);
    expect(resp.status).toBe(200);
    const j = (await resp.json()) as { url: string };
    expect(j.url).toBe(portalUrl);

    // Verify the call to Stripe was well-formed.
    const fetchMock = fetch as unknown as ReturnType<typeof vi.fn>;
    expect(fetchMock).toHaveBeenCalledOnce();
    const [calledUrl, init] = fetchMock.mock.calls[0];
    expect(calledUrl).toBe(
      "https://api.stripe.com/v1/billing_portal/sessions"
    );
    expect((init as RequestInit).method).toBe("POST");
    const body = (init as RequestInit).body as string;
    expect(body).toContain("customer=cus_test_abc");
    expect(body).toContain("return_url=");
  });

  test("502 when Stripe returns an error", async () => {
    const state = emptyState();
    const claims = await freshClaims();
    state.licenses.set(claims.lid, {
      license_id: claims.lid,
      email_hash: claims.eh,
      tier: "annual",
      issued_at: claims.iat,
      valid_until: claims.exp,
      max_devices: 5,
      status: "active",
      stripe_session_id: "cs_x",
      stripe_customer_id: "cus_dead",
      stripe_subscription_id: "sub_x",
      current_period_end: claims.exp,
      cancel_at_period_end: 0,
    });
    const token = await signToken(claims, priv);

    (fetch as unknown as ReturnType<typeof vi.fn>).mockResolvedValueOnce(
      new Response("No such customer: cus_dead", { status: 404 })
    );

    const req = new Request("http://localhost/api/billing-portal", {
      method: "POST",
      body: JSON.stringify({ token }),
    });
    const resp = await handleBillingPortal(req, makeEnv(state, priv, pub), ctx);
    expect(resp.status).toBe(502);
  });

  test("custom return_url is honoured when https/dimmy", async () => {
    const state = emptyState();
    const claims = await freshClaims();
    state.licenses.set(claims.lid, {
      license_id: claims.lid,
      email_hash: claims.eh,
      tier: "monthly",
      issued_at: claims.iat,
      valid_until: claims.exp,
      max_devices: 5,
      status: "active",
      stripe_session_id: "cs",
      stripe_customer_id: "cus_ok",
      stripe_subscription_id: "sub",
      current_period_end: claims.exp,
      cancel_at_period_end: 0,
    });
    const token = await signToken(claims, priv);
    (fetch as unknown as ReturnType<typeof vi.fn>).mockResolvedValueOnce(
      new Response(JSON.stringify({ url: "https://billing.stripe.com/x" }), {
        status: 200,
      })
    );

    const req = new Request("http://localhost/api/billing-portal", {
      method: "POST",
      body: JSON.stringify({ token, return_url: "dimmy://license" }),
    });
    await handleBillingPortal(req, makeEnv(state, priv, pub), ctx);

    const fetchMock = fetch as unknown as ReturnType<typeof vi.fn>;
    const body = (fetchMock.mock.calls[0][1] as RequestInit).body as string;
    expect(decodeURIComponent(body)).toContain("return_url=dimmy://license");
  });

  test("javascript: return_url is rejected (falls back to default)", async () => {
    const state = emptyState();
    const claims = await freshClaims();
    state.licenses.set(claims.lid, {
      license_id: claims.lid,
      email_hash: claims.eh,
      tier: "monthly",
      issued_at: claims.iat,
      valid_until: claims.exp,
      max_devices: 5,
      status: "active",
      stripe_session_id: "cs",
      stripe_customer_id: "cus_ok",
      stripe_subscription_id: "sub",
      current_period_end: claims.exp,
      cancel_at_period_end: 0,
    });
    const token = await signToken(claims, priv);
    (fetch as unknown as ReturnType<typeof vi.fn>).mockResolvedValueOnce(
      new Response(JSON.stringify({ url: "https://x" }), { status: 200 })
    );
    const req = new Request("http://localhost/api/billing-portal", {
      method: "POST",
      body: JSON.stringify({
        token,
        return_url: "javascript:alert(1)",
      }),
    });
    await handleBillingPortal(req, makeEnv(state, priv, pub), ctx);
    const body = (
      (fetch as unknown as ReturnType<typeof vi.fn>).mock.calls[0][1] as RequestInit
    ).body as string;
    expect(decodeURIComponent(body)).not.toContain("javascript:");
    expect(decodeURIComponent(body)).toContain("return_url=https://example.test/portal-return");
  });
});
