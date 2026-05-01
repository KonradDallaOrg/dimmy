// /api/devices/list + /api/devices/deactivate — auth via Ed25519
// token signature (no API key). The list endpoint shows all devices on
// the caller's license (active + revoked) so users can see history;
// the deactivate endpoint flips one to revoked, freeing a slot.

import { describe, expect, test } from "vitest";
import {
  handleDeviceDeactivate,
  handleDevicesList,
} from "../src/handlers/devices";
import { signToken, type Claims } from "../src/crypto";
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

async function setup() {
  const kp = await makeKeypair();
  const state = emptyState();
  state.licenses.set("lic_d", {
    license_id: "lic_d", email_hash: "eh", tier: "annual",
    issued_at: 1000, valid_until: 100_000_000,
    max_devices: 5, status: "active",
    stripe_session_id: null, stripe_customer_id: null,
    stripe_subscription_id: null, current_period_end: null,
    cancel_at_period_end: 0,
  });
  // 3 devices: two active (one is "self"), one already revoked.
  state.devices.set("did_self", {
    device_id: "did_self", license_id: "lic_d", device_label: "MyMac",
    issued_at: 1000, last_seen: 1000, status: "active",
  });
  state.devices.set("did_other", {
    device_id: "did_other", license_id: "lic_d", device_label: "Spare",
    issued_at: 1000, last_seen: 1000, status: "active",
  });
  state.devices.set("did_old", {
    device_id: "did_old", license_id: "lic_d", device_label: "Old",
    issued_at: 1000, last_seen: 1000, status: "revoked",
  });
  const env: Env = {
    DB: makeMockDB(state) as unknown as D1Database,
    PUBLIC_URL: "http://localhost:8787",
    EMAIL_FROM: "Dimmy <hello@dimmy.app>",
    STRIPE_PRICE_MONTHLY: "price_m",
    STRIPE_PRICE_ANNUAL: "price_a",
    STRIPE_PRICE_LIFETIME: "price_l",
    DIMMY_LICENSE_PRIVKEY: kp.priv,
    DIMMY_LICENSE_PUBKEY: kp.pub,
    STRIPE_WEBHOOK_SECRET: "whsec_test",
    STRIPE_SECRET_KEY: "sk_test",
    RESEND_API_KEY: "",
  };
  const claims: Claims = {
    v: 1, lid: "lic_d", eh: "eh", tier: "annual",
    iat: 1000, exp: 100_000_000, max_offline: 30,
    did: "did_self", scope: ["managed_stt"],
  };
  const token = await signToken(claims, kp.priv);
  return { state, env, kp, token };
}

const ctx = {} as ExecutionContext;

function makeReq(path: "list" | "deactivate", body: unknown): Request {
  return new Request(`http://localhost/api/devices/${path}`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body),
  });
}

describe("/api/devices/list", () => {
  test("400 when token missing", async () => {
    const { env } = await setup();
    const resp = await handleDevicesList(makeReq("list", {}), env, ctx);
    expect(resp.status).toBe(400);
  });

  test("400 when token invalid", async () => {
    const { env } = await setup();
    const resp = await handleDevicesList(
      makeReq("list", { token: "bad.token" }), env, ctx
    );
    expect(resp.status).toBe(400);
  });

  test("returns only ACTIVE devices (revoked are hidden — see commit 5a30904)", async () => {
    // Per UX decision in 5a30904: listing should not show ghosts.
    // Revoked devices stay in the DB for audit but disappear from UI.
    const { env, token } = await setup();
    const resp = await handleDevicesList(makeReq("list", { token }), env, ctx);
    expect(resp.status).toBe(200);
    const body = await resp.json() as { devices: any[]; max_devices: number };
    expect(body.max_devices).toBe(5);
    // 2 active devices: self + other. Revoked did_old NOT in the list.
    expect(body.devices.length).toBe(2);
    expect(body.devices.find((d) => d.device_id === "did_old")).toBeUndefined();
    const self = body.devices.find((d) => d.device_id === "did_self");
    expect(self.is_self).toBe(true);
    const other = body.devices.find((d) => d.device_id === "did_other");
    expect(other.is_self).toBe(false);
  });
});

describe("/api/devices/deactivate", () => {
  test("400 when token missing", async () => {
    const { env } = await setup();
    const resp = await handleDeviceDeactivate(
      makeReq("deactivate", {}), env, ctx
    );
    expect(resp.status).toBe(400);
  });

  test("self-sign-out (no device_id) revokes the calling device", async () => {
    const { env, state, token } = await setup();
    const resp = await handleDeviceDeactivate(
      makeReq("deactivate", { token }), env, ctx
    );
    expect(resp.status).toBe(200);
    expect(state.devices.get("did_self")!.status).toBe("revoked");
    // Other device untouched.
    expect(state.devices.get("did_other")!.status).toBe("active");
  });

  test("deactivate another device on the same license", async () => {
    const { env, state, token } = await setup();
    const resp = await handleDeviceDeactivate(
      makeReq("deactivate", { token, device_id: "did_other" }), env, ctx
    );
    expect(resp.status).toBe(200);
    expect(state.devices.get("did_other")!.status).toBe("revoked");
    // Self stays active.
    expect(state.devices.get("did_self")!.status).toBe("active");
  });

  test("404 when target device not on this license", async () => {
    const { env, token } = await setup();
    const resp = await handleDeviceDeactivate(
      makeReq("deactivate", { token, device_id: "did_other_account" }),
      env, ctx
    );
    expect(resp.status).toBe(404);
  });

  test("404 when target device already revoked (idempotency: rows-changed=0)", async () => {
    const { env, token } = await setup();
    // did_old is already revoked in setup.
    const resp = await handleDeviceDeactivate(
      makeReq("deactivate", { token, device_id: "did_old" }), env, ctx
    );
    expect(resp.status).toBe(404);
  });

  test("audit row written on successful deactivate", async () => {
    const { env, state, token } = await setup();
    await handleDeviceDeactivate(
      makeReq("deactivate", { token, device_id: "did_other" }), env, ctx
    );
    const audits = state.audit_log.filter(
      (a) => a.event_type === "device_deactivated"
    );
    expect(audits.length).toBe(1);
  });
});
