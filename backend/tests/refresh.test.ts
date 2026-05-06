// /api/refresh — bumps device.last_seen, re-issues a fresh token with
// updated iat (same lid + did + exp). Used by clients every ~24h to
// keep last_online_check in sync so the offline-grace clock is honest.

import { describe, expect, test } from "vitest";
import { handleRefresh } from "../src/handlers/refresh";
import { signToken, verifyTokenWithPub, type Claims } from "../src/crypto";
import { emptyState, makeMockDB, type MockState } from "./_d1-mock";
import type { Env } from "../src/index";

async function makeKeypair(): Promise<{ priv: string; pub: string }> {
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

async function makeEnv(state: MockState, kp?: { priv: string; pub: string }): Promise<Env> {
  const k = kp ?? await makeKeypair();
  return {
    DB: makeMockDB(state) as unknown as D1Database,
    PUBLIC_URL: "http://localhost:8787",
    EMAIL_FROM: "Dimmy <hello@dimmy.app>",
    STRIPE_PRICE_MONTHLY: "price_m",
    STRIPE_PRICE_ANNUAL: "price_a",
    STRIPE_PRICE_LIFETIME: "price_l",
    DIMMY_LICENSE_PRIVKEY: k.priv,
    DIMMY_LICENSE_PUBKEY: k.pub,
    STRIPE_WEBHOOK_SECRET: "whsec_test",
    STRIPE_SECRET_KEY: "sk_test",
    RESEND_API_KEY: "",
  };
}

const ctx = {} as ExecutionContext;

function seedLicense(state: MockState, id: string, status: "active" | "past_due" | "revoked" | "deleted" = "active") {
  state.licenses.set(id, {
    license_id: id, email_hash: "eh", tier: "annual",
    issued_at: 1000, valid_until: 100_000_000,
    max_devices: 5, status,
    stripe_session_id: null, stripe_customer_id: null,
    stripe_subscription_id: null, current_period_end: null,
    cancel_at_period_end: 0,
  });
}

function seedDevice(state: MockState, id: string, licenseId: string, status: "active" | "revoked" = "active") {
  state.devices.set(id, {
    device_id: id, license_id: licenseId,
    device_label: "Dev", issued_at: 1000, last_seen: 1000, status,
  });
}

function makeReq(body: unknown): Request {
  return new Request("http://localhost/api/refresh", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body),
  });
}

describe("/api/refresh", () => {
  test("400 when token missing", async () => {
    const env = await makeEnv(emptyState());
    const resp = await handleRefresh(makeReq({}), env, ctx);
    expect(resp.status).toBe(400);
  });

  test("400 when token invalid (signature mismatch)", async () => {
    const env = await makeEnv(emptyState());
    const resp = await handleRefresh(
      makeReq({ token: "garbage.invalid.token" }), env, ctx
    );
    expect(resp.status).toBe(400);
  });

  test("404 when license_id in token not in DB", async () => {
    const kp = await makeKeypair();
    const state = emptyState();
    const env = await makeEnv(state, kp);
    const claims: Claims = {
      v: 1, lid: "lic_missing", eh: "eh", tier: "annual",
      iat: 1000, exp: 100_000_000, max_offline: 30,
      did: "did_x", scope: ["managed_stt"],
    };
    const token = await signToken(claims, kp.priv);
    const resp = await handleRefresh(makeReq({ token }), env, ctx);
    expect(resp.status).toBe(404);
  });

  test("409 when license is revoked", async () => {
    const kp = await makeKeypair();
    const state = emptyState();
    seedLicense(state, "lic_rev", "revoked");
    seedDevice(state, "did_rev", "lic_rev");
    const env = await makeEnv(state, kp);
    const claims: Claims = {
      v: 1, lid: "lic_rev", eh: "eh", tier: "annual",
      iat: 1000, exp: 100_000_000, max_offline: 30,
      did: "did_rev", scope: ["managed_stt"],
    };
    const token = await signToken(claims, kp.priv);
    const resp = await handleRefresh(makeReq({ token }), env, ctx);
    expect(resp.status).toBe(409);
  });

  test("404 when device in token not in DB", async () => {
    const kp = await makeKeypair();
    const state = emptyState();
    seedLicense(state, "lic_nd", "active");
    const env = await makeEnv(state, kp);
    const claims: Claims = {
      v: 1, lid: "lic_nd", eh: "eh", tier: "annual",
      iat: 1000, exp: 100_000_000, max_offline: 30,
      did: "did_unknown", scope: ["managed_stt"],
    };
    const token = await signToken(claims, kp.priv);
    const resp = await handleRefresh(makeReq({ token }), env, ctx);
    expect(resp.status).toBe(404);
  });

  test("409 when device is deactivated (revoked status)", async () => {
    const kp = await makeKeypair();
    const state = emptyState();
    seedLicense(state, "lic_dd", "active");
    seedDevice(state, "did_dd", "lic_dd", "revoked");
    const env = await makeEnv(state, kp);
    const claims: Claims = {
      v: 1, lid: "lic_dd", eh: "eh", tier: "annual",
      iat: 1000, exp: 100_000_000, max_offline: 30,
      did: "did_dd", scope: ["managed_stt"],
    };
    const token = await signToken(claims, kp.priv);
    const resp = await handleRefresh(makeReq({ token }), env, ctx);
    expect(resp.status).toBe(409);
  });

  test("happy path → bumps last_seen + returns fresh token", async () => {
    const kp = await makeKeypair();
    const state = emptyState();
    seedLicense(state, "lic_h", "active");
    seedDevice(state, "did_h", "lic_h");
    const env = await makeEnv(state, kp);
    const claims: Claims = {
      v: 1, lid: "lic_h", eh: "eh", tier: "annual",
      iat: 1000, exp: 100_000_000, max_offline: 30,
      did: "did_h", scope: ["managed_stt"],
    };
    const token = await signToken(claims, kp.priv);
    const resp = await handleRefresh(makeReq({ token }), env, ctx);
    expect(resp.status).toBe(200);

    const body = await resp.json() as { token: string };
    const fresh = await verifyTokenWithPub(body.token, kp.pub);
    expect(fresh.lid).toBe("lic_h");
    expect(fresh.did).toBe("did_h");
    // iat must be newer than the original.
    expect(fresh.iat).toBeGreaterThan(claims.iat);
    // exp must match the license's valid_until.
    expect(fresh.exp).toBe(state.licenses.get("lic_h")!.valid_until);
    // last_seen must be bumped on the device.
    expect(state.devices.get("did_h")!.last_seen).toBeGreaterThan(1000);
  });

  test("cancels_at populated on refresh when license has cancel flag set", async () => {
    const kp = await makeKeypair();
    const state = emptyState();
    seedLicense(state, "lic_cxl", "active");
    state.licenses.get("lic_cxl")!.cancel_at_period_end = 1;
    state.licenses.get("lic_cxl")!.current_period_end = 1888888888;
    seedDevice(state, "did_cxl", "lic_cxl");
    const env = await makeEnv(state, kp);
    const claims: Claims = {
      v: 1, lid: "lic_cxl", eh: "eh", tier: "annual",
      iat: 1000, exp: 100_000_000, max_offline: 30,
      did: "did_cxl", scope: ["managed_stt"],
    };
    const token = await signToken(claims, kp.priv);
    const resp = await handleRefresh(makeReq({ token }), env, ctx);
    expect(resp.status).toBe(200);
    const fresh = await verifyTokenWithPub(
      ((await resp.json()) as { token: string }).token, kp.pub
    );
    expect(fresh.cancels_at).toBe(1888888888);
  });

  test("scope refresh from server tier table, not from inbound claim", async () => {
    // Server-side tier→scope is the authoritative mapping. If the
    // operator adds a scope to a tier, the next refresh propagates it
    // to the client without a client release. We verify by setting an
    // inbound token with EMPTY scope and checking the refreshed token
    // contains the full set.
    const kp = await makeKeypair();
    const state = emptyState();
    seedLicense(state, "lic_sc", "active");
    seedDevice(state, "did_sc", "lic_sc");
    const env = await makeEnv(state, kp);
    const claims: Claims = {
      v: 1, lid: "lic_sc", eh: "eh", tier: "annual",
      iat: 1000, exp: 100_000_000, max_offline: 30,
      did: "did_sc", scope: [], // empty intentionally
    };
    const token = await signToken(claims, kp.priv);
    const resp = await handleRefresh(makeReq({ token }), env, ctx);
    expect(resp.status).toBe(200);
    const fresh = await verifyTokenWithPub(
      ((await resp.json()) as { token: string }).token, kp.pub
    );
    // Annual scope set must include at least these two.
    expect(fresh.scope).toContain("managed_stt");
    expect(fresh.scope).toContain("managed_llm");
  });
});
