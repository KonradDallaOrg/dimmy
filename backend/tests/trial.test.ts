// /api/trial/start — provisions a 14-day trial. Idempotent for the
// email (re-issuing a code does NOT extend validity). Verifies the
// trial-reset-prevention property from PoC scenario #7.

import { describe, expect, test } from "vitest";
import { handleTrialStart } from "../src/handlers/trial";
import { emailHash } from "../src/crypto";
import { emptyState, makeMockDB, type MockState } from "./_d1-mock";
import type { Env } from "../src/index";

function makeEnv(state: MockState): Env {
  return {
    DB: makeMockDB(state) as unknown as D1Database,
    PUBLIC_URL: "http://localhost:8787",
    EMAIL_FROM: "Dimmy <hello@dimmy.app>",
    STRIPE_PRICE_MONTHLY: "price_m",
    STRIPE_PRICE_ANNUAL: "price_a",
    STRIPE_PRICE_LIFETIME: "price_l",
    DIMMY_LICENSE_PRIVKEY: "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
    DIMMY_LICENSE_PUBKEY:  "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
    STRIPE_WEBHOOK_SECRET: "whsec_test",
    STRIPE_SECRET_KEY: "sk_test",
    RESEND_API_KEY: "",
  };
}

const ctx = {} as ExecutionContext;

function makeReq(body: unknown): Request {
  return new Request("http://localhost/api/trial/start", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: typeof body === "string" ? body : JSON.stringify(body),
  });
}

describe("/api/trial/start", () => {
  test("400 when email missing", async () => {
    const resp = await handleTrialStart(makeReq({}), makeEnv(emptyState()), ctx);
    expect(resp.status).toBe(400);
  });

  test("400 when email malformed (no @)", async () => {
    const resp = await handleTrialStart(
      makeReq({ email: "notanemail" }), makeEnv(emptyState()), ctx
    );
    expect(resp.status).toBe(400);
  });

  test("400 when body is not JSON", async () => {
    const resp = await handleTrialStart(
      makeReq("{not json"), makeEnv(emptyState()), ctx
    );
    expect(resp.status).toBe(400);
  });

  test("fresh email → creates trial license + activation code", async () => {
    const state = emptyState();
    const resp = await handleTrialStart(
      makeReq({ email: "alice@dev.local" }), makeEnv(state), ctx
    );
    expect(resp.status).toBe(200);
    const body = await resp.json() as { magic_link: string; code: string };
    // trial.ts deliberately uses the HTTPS bridge URL (PUBLIC_URL/activate?...)
    // because Gmail / Outlook strip custom URL schemes from email links.
    // The bridge page itself JS-redirects to dimmy:// once opened.
    expect(body.magic_link).toMatch(/\/activate\?code=/);
    expect(body.code).toMatch(/^[A-Za-z0-9]{8,64}$/);
    // Exactly one license created.
    expect(state.licenses.size).toBe(1);
    const lic = [...state.licenses.values()][0];
    expect(lic.tier).toBe("trial");
    expect(lic.status).toBe("active");
    // Exactly one activation code minted.
    expect(state.activation_codes.size).toBe(1);
  });

  test("email is case-normalised + trimmed before hashing", async () => {
    const state = emptyState();
    await handleTrialStart(
      makeReq({ email: "  Alice@DEV.local  " }), makeEnv(state), ctx
    );
    const lic = [...state.licenses.values()][0];
    expect(lic.email_hash).toBe(await emailHash("alice@dev.local"));
  });

  test("idempotent: same email → same license_id, valid_until UNCHANGED, code dedup'd within window", async () => {
    // Pre-dedup behaviour: every call minted a fresh code. Post-dedup:
    // within MAGIC_LINK_DEDUP_SECS (5 min), the unconsumed code is
    // re-used so the user doesn't get two emails with two different
    // codes. The license-level idempotency (same lid, same
    // valid_until) is unchanged.
    const state = emptyState();
    const r1 = await handleTrialStart(
      makeReq({ email: "bob@dev.local" }), makeEnv(state), ctx
    );
    expect(r1.status).toBe(200);
    const j1 = (await r1.json()) as { code: string };
    const lic1 = [...state.licenses.values()][0];
    const originalValidUntil = lic1.valid_until;

    await new Promise((r) => setTimeout(r, 5));

    const r2 = await handleTrialStart(
      makeReq({ email: "bob@dev.local" }), makeEnv(state), ctx
    );
    expect(r2.status).toBe(200);
    const j2 = (await r2.json()) as { code: string };

    // Still exactly one license (not 2).
    expect(state.licenses.size).toBe(1);
    expect([...state.licenses.values()][0].license_id).toBe(lic1.license_id);
    // Validity NOT reset to a fresh 14 days.
    expect([...state.licenses.values()][0].valid_until).toBe(originalValidUntil);
    // Same code re-issued (dedup), NOT a second row in activation_codes.
    expect(j2.code).toBe(j1.code);
    expect(state.activation_codes.size).toBe(1);
  });

  test("expired trial → 409 (cannot just delete file to reset)", async () => {
    // Trial-reset-prevention: scenario #7 from docs/dev/licensing-poc.md.
    // The user has consumed their 14 days; we don't silently grant them a
    // fresh trial just because they pinged /api/trial/start again.
    const state = emptyState();
    const eh = await emailHash("expired@dev.local");
    state.licenses.set("lic_expired", {
      license_id: "lic_expired", email_hash: eh, tier: "trial",
      issued_at: 1, valid_until: 2, // Expired long ago.
      max_devices: 5, status: "active",
      stripe_session_id: null, stripe_customer_id: null,
      stripe_subscription_id: null, current_period_end: null,
      cancel_at_period_end: 0,
    });
    const resp = await handleTrialStart(
      makeReq({ email: "expired@dev.local" }), makeEnv(state), ctx
    );
    expect(resp.status).toBe(409);
    // No new license was created.
    expect(state.licenses.size).toBe(1);
  });

  test("paid license already exists → re-issues code for that paid license, no trial spin-up", async () => {
    const state = emptyState();
    const eh = await emailHash("paid@dev.local");
    state.licenses.set("lic_paid", {
      license_id: "lic_paid", email_hash: eh, tier: "annual",
      issued_at: 1000, valid_until: 100_000_000,
      max_devices: 5, status: "active",
      stripe_session_id: "cs", stripe_customer_id: "cus",
      stripe_subscription_id: "sub", current_period_end: 100_000_000,
      cancel_at_period_end: 0,
    });
    const resp = await handleTrialStart(
      makeReq({ email: "paid@dev.local" }), makeEnv(state), ctx
    );
    expect(resp.status).toBe(200);
    // Still annual, no new trial license.
    expect(state.licenses.size).toBe(1);
    expect([...state.licenses.values()][0].tier).toBe("annual");
    // A code was minted for the paid license.
    expect([...state.activation_codes.values()][0].license_id).toBe("lic_paid");
  });

  test("magic-link dedup: two start-trial calls within 5 min reuse the same code", async () => {
    // Catches the "user double-clicks Start Trial / page reloads twice"
    // failure mode where each click would otherwise mint a fresh code +
    // send a fresh email. Both arrive but with DIFFERENT codes — the
    // user sees two emails, doesn't know which to click.
    const state = emptyState();
    const r1 = await handleTrialStart(
      makeReq({ email: "dedup@example.com" }),
      makeEnv(state),
      ctx
    );
    expect(r1.status).toBe(200);
    const j1 = (await r1.json()) as { code: string };
    expect(state.activation_codes.size).toBe(1);

    const r2 = await handleTrialStart(
      makeReq({ email: "dedup@example.com" }),
      makeEnv(state),
      ctx
    );
    expect(r2.status).toBe(200);
    const j2 = (await r2.json()) as { code: string };

    // SAME code returned, no second activation_code row.
    expect(j2.code).toBe(j1.code);
    expect(state.activation_codes.size).toBe(1);
    // Single license (idempotent on email).
    expect(state.licenses.size).toBe(1);
  });

  test("magic-link dedup: a fresh code is minted after the first one is consumed", async () => {
    // Once the user actually uses the code, future calls must mint a
    // new one — the dedup window only protects against accidental
    // double-issuance of UNCONSUMED codes.
    const state = emptyState();
    const r1 = await handleTrialStart(
      makeReq({ email: "consumed@example.com" }),
      makeEnv(state),
      ctx
    );
    const j1 = (await r1.json()) as { code: string };
    // Simulate consumption.
    const c = [...state.activation_codes.values()][0];
    c.consumed_at = Math.floor(Date.now() / 1000);

    const r2 = await handleTrialStart(
      makeReq({ email: "consumed@example.com" }),
      makeEnv(state),
      ctx
    );
    const j2 = (await r2.json()) as { code: string };
    expect(j2.code).not.toBe(j1.code);
    expect(state.activation_codes.size).toBe(2);
  });
});
