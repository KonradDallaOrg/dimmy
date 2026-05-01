// /api/activate?code=&device_label= — exchange a one-time code for a
// signed token. Tests the full happy path (200 + verifiable token) plus
// every rejection: unknown / consumed / expired / device-limit /
// suspended-license. The crypto fixtures use a real Ed25519 keypair so
// we can verify the issued token round-trips through the Worker's own
// verifyTokenWithPub.

import { describe, expect, test } from "vitest";
import { handleActivate } from "../src/handlers/activate";
import { signToken, verifyTokenWithPub } from "../src/crypto";
import { emptyState, makeMockDB, type MockState } from "./_d1-mock";
import type { Env } from "../src/index";

async function makeKeypair(): Promise<{ priv: string; pub: string }> {
  const kp = (await crypto.subtle.generateKey(
    { name: "Ed25519" } as EcKeyGenParams,
    true,
    ["sign", "verify"]
  )) as CryptoKeyPair;
  const pkcs8 = new Uint8Array(await crypto.subtle.exportKey("pkcs8", kp.privateKey));
  const seed = pkcs8.slice(16, 48);
  const spki = new Uint8Array(await crypto.subtle.exportKey("spki", kp.publicKey));
  const pubRaw = spki.slice(12, 44);
  const b64u = (b: Uint8Array) => Buffer.from(b).toString("base64url");
  return { priv: b64u(seed), pub: b64u(pubRaw) };
}

async function makeEnv(state: MockState): Promise<Env> {
  const { priv, pub } = await makeKeypair();
  return {
    DB: makeMockDB(state) as unknown as D1Database,
    PUBLIC_URL: "http://localhost:8787",
    EMAIL_FROM: "Dimmy <hello@dimmy.app>",
    STRIPE_PRICE_MONTHLY: "price_m",
    STRIPE_PRICE_ANNUAL: "price_a",
    STRIPE_PRICE_LIFETIME: "price_l",
    DIMMY_LICENSE_PRIVKEY: priv,
    DIMMY_LICENSE_PUBKEY: pub,
    STRIPE_WEBHOOK_SECRET: "whsec_test",
    STRIPE_SECRET_KEY: "sk_test",
    RESEND_API_KEY: "",
  };
}

const ctx = {} as ExecutionContext;

function seedActiveLicense(state: MockState, licenseId: string, tier: "trial" | "monthly" | "annual" | "lifetime") {
  state.licenses.set(licenseId, {
    license_id: licenseId,
    email_hash: "eh_" + licenseId,
    tier,
    issued_at: 1000,
    valid_until: 1000 + 365 * 86400,
    max_devices: 5,
    status: "active",
    stripe_session_id: null,
    stripe_customer_id: null,
    stripe_subscription_id: null,
    current_period_end: null,
    cancel_at_period_end: 0,
  });
}

function seedActivationCode(state: MockState, code: string, licenseId: string, opts: { consumed?: boolean; expired?: boolean } = {}) {
  const now = Math.floor(Date.now() / 1000);
  state.activation_codes.set(code, {
    code,
    license_id: licenseId,
    created_at: now - 60,
    expires_at: opts.expired ? now - 1 : now + 600,
    consumed_at: opts.consumed ? now - 30 : null,
  });
}

function makeReq(code: string | null, deviceLabel?: string): Request {
  const params = new URLSearchParams();
  if (code !== null) params.set("code", code);
  if (deviceLabel !== undefined) params.set("device_label", deviceLabel);
  return new Request(`http://localhost/api/activate?${params}`);
}

describe("/api/activate", () => {
  test("400 when code missing", async () => {
    const env = await makeEnv(emptyState());
    const resp = await handleActivate(makeReq(null), env, ctx);
    expect(resp.status).toBe(400);
  });

  test("404 when code unknown", async () => {
    const env = await makeEnv(emptyState());
    const resp = await handleActivate(makeReq("nope"), env, ctx);
    expect(resp.status).toBe(404);
  });

  test("409 when code already consumed", async () => {
    const state = emptyState();
    seedActiveLicense(state, "lic1", "annual");
    seedActivationCode(state, "code1", "lic1", { consumed: true });
    const env = await makeEnv(state);
    const resp = await handleActivate(makeReq("code1"), env, ctx);
    expect(resp.status).toBe(409);
  });

  test("409 when code expired", async () => {
    const state = emptyState();
    seedActiveLicense(state, "lic2", "annual");
    seedActivationCode(state, "code2", "lic2", { expired: true });
    const env = await makeEnv(state);
    const resp = await handleActivate(makeReq("code2"), env, ctx);
    expect(resp.status).toBe(409);
  });

  test("happy path → 200 + verifiable token + device created + code consumed", async () => {
    const state = emptyState();
    seedActiveLicense(state, "lic_ok", "annual");
    seedActivationCode(state, "code_ok", "lic_ok");
    const env = await makeEnv(state);
    const resp = await handleActivate(makeReq("code_ok", "MyMac"), env, ctx);
    expect(resp.status).toBe(200);
    const body = await resp.json() as { token: string };
    // Token round-trips through verify with the issuing pubkey.
    const claims = await verifyTokenWithPub(body.token, env.DIMMY_LICENSE_PUBKEY);
    expect(claims.tier).toBe("annual");
    expect(claims.lid).toBe("lic_ok");
    expect(claims.scope).toContain("managed_stt");
    // Device row inserted with the requested label.
    const devices = [...state.devices.values()];
    expect(devices.length).toBe(1);
    expect(devices[0].device_label).toBe("MyMac");
    expect(devices[0].status).toBe("active");
    // Code marked consumed (so a replay returns 409).
    expect(state.activation_codes.get("code_ok")!.consumed_at).not.toBeNull();
  });

  test("default device_label when omitted", async () => {
    const state = emptyState();
    seedActiveLicense(state, "lic_d", "monthly");
    seedActivationCode(state, "code_d", "lic_d");
    const env = await makeEnv(state);
    const resp = await handleActivate(makeReq("code_d"), env, ctx);
    expect(resp.status).toBe(200);
    const dev = [...state.devices.values()][0];
    expect(dev.device_label).toBe("Unknown device");
  });

  test("409 when license is suspended", async () => {
    const state = emptyState();
    seedActiveLicense(state, "lic_susp", "annual");
    state.licenses.get("lic_susp")!.status = "revoked";
    seedActivationCode(state, "code_s", "lic_susp");
    const env = await makeEnv(state);
    const resp = await handleActivate(makeReq("code_s"), env, ctx);
    expect(resp.status).toBe(409);
  });

  test("429 when device limit reached (5 active devices already)", async () => {
    const state = emptyState();
    seedActiveLicense(state, "lic_full", "annual");
    seedActivationCode(state, "code_full", "lic_full");
    for (let i = 0; i < 5; i++) {
      state.devices.set(`d_${i}`, {
        device_id: `d_${i}`,
        license_id: "lic_full",
        device_label: `Dev ${i}`,
        issued_at: 100,
        last_seen: 100,
        status: "active",
      });
    }
    const env = await makeEnv(state);
    const resp = await handleActivate(makeReq("code_full"), env, ctx);
    expect(resp.status).toBe(429);
    // Code MUST NOT be consumed when activate fails.
    expect(state.activation_codes.get("code_full")!.consumed_at).toBeNull();
  });

  test("revoked devices DON'T count against the 5-device limit", async () => {
    const state = emptyState();
    seedActiveLicense(state, "lic_some", "annual");
    seedActivationCode(state, "code_some", "lic_some");
    // 4 active + 3 revoked. Total 7 rows, but only 4 are active.
    for (let i = 0; i < 4; i++) {
      state.devices.set(`a_${i}`, {
        device_id: `a_${i}`, license_id: "lic_some",
        device_label: `Active ${i}`, issued_at: 100, last_seen: 100,
        status: "active",
      });
    }
    for (let i = 0; i < 3; i++) {
      state.devices.set(`r_${i}`, {
        device_id: `r_${i}`, license_id: "lic_some",
        device_label: `Revoked ${i}`, issued_at: 100, last_seen: 100,
        status: "revoked",
      });
    }
    const env = await makeEnv(state);
    const resp = await handleActivate(makeReq("code_some"), env, ctx);
    expect(resp.status).toBe(200);
  });

  test("scopes in token reflect the tier mapping", async () => {
    // Lifetime should include managed_stt + managed_llm + auto_update +
    // history_sync + premium_styles. Monthly excludes history_sync.
    for (const tier of ["monthly", "annual", "lifetime"] as const) {
      const state = emptyState();
      seedActiveLicense(state, `lic_${tier}`, tier);
      seedActivationCode(state, `c_${tier}`, `lic_${tier}`);
      const env = await makeEnv(state);
      const resp = await handleActivate(makeReq(`c_${tier}`), env, ctx);
      expect(resp.status).toBe(200);
      const body = await resp.json() as { token: string };
      const claims = await verifyTokenWithPub(body.token, env.DIMMY_LICENSE_PUBKEY);
      expect(claims.scope).toContain("managed_stt");
      expect(claims.scope).toContain("managed_llm");
      expect(claims.tier).toBe(tier);
      if (tier === "monthly") {
        expect(claims.scope).not.toContain("history_sync");
      } else {
        expect(claims.scope).toContain("history_sync");
      }
    }
  });

  test("token exp matches license valid_until", async () => {
    const state = emptyState();
    seedActiveLicense(state, "lic_exp", "annual");
    state.licenses.get("lic_exp")!.valid_until = 9999999;
    seedActivationCode(state, "c_exp", "lic_exp");
    const env = await makeEnv(state);
    const resp = await handleActivate(makeReq("c_exp"), env, ctx);
    expect(resp.status).toBe(200);
    const body = await resp.json() as { token: string };
    const claims = await verifyTokenWithPub(body.token, env.DIMMY_LICENSE_PUBKEY);
    expect(claims.exp).toBe(9999999);
  });
});
