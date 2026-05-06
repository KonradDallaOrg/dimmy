// /api/account/delete — GDPR right-to-erasure two-step OTP flow.
// Compliance-critical: an unauthenticated caller MUST NOT be able to
// wipe data, and we MUST keep an audit trail (Recital 26 anonymous
// data is fine, dropping rows that prove we honoured a request is not).

import { describe, expect, test } from "vitest";
import { handleAccountDelete } from "../src/handlers/delete";
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
  return new Request("http://localhost/api/account/delete", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: typeof body === "string" ? body : JSON.stringify(body),
  });
}

async function seedLicensedAccount(state: MockState, email: string): Promise<{ eh: string; lid: string }> {
  const eh = await emailHash(email);
  const lid = "lic_del_" + email;
  state.licenses.set(lid, {
    license_id: lid, email_hash: eh, tier: "annual",
    issued_at: 1000, valid_until: 100_000_000,
    max_devices: 5, status: "active",
    stripe_session_id: null, stripe_customer_id: null,
    stripe_subscription_id: null, current_period_end: null,
    cancel_at_period_end: 0,
  });
  return { eh, lid };
}

describe("/api/account/delete (GDPR)", () => {
  test("400 when email missing", async () => {
    const resp = await handleAccountDelete(makeReq({}), makeEnv(emptyState()), ctx);
    expect(resp.status).toBe(400);
  });

  test("400 when email malformed", async () => {
    const resp = await handleAccountDelete(
      makeReq({ email: "garbage" }), makeEnv(emptyState()), ctx
    );
    expect(resp.status).toBe(400);
  });

  test("invalid JSON body → 400", async () => {
    const resp = await handleAccountDelete(
      makeReq("{not json"), makeEnv(emptyState()), ctx
    );
    expect(resp.status).toBe(400);
  });

  test("STEP 1: unknown email returns generic 'if-exists, OTP sent' (don't leak)", async () => {
    // Defence against email enumeration: we can't reveal whether an
    // email is on file. Same shape for known + unknown.
    const state = emptyState();
    const resp = await handleAccountDelete(
      makeReq({ email: "ghost@nowhere.invalid" }), makeEnv(state), ctx
    );
    expect(resp.status).toBe(200);
    const body = await resp.json() as { status: string };
    expect(body.status).toMatch(/sent/i);
    // Crucial: NO license created, NO code minted for the unknown email.
    expect(state.licenses.size).toBe(0);
    expect(state.activation_codes.size).toBe(0);
  });

  test("STEP 1: known email → mint OTP code + audit", async () => {
    const state = emptyState();
    const { lid } = await seedLicensedAccount(state, "alice@dev.local");
    const resp = await handleAccountDelete(
      makeReq({ email: "alice@dev.local" }), makeEnv(state), ctx
    );
    expect(resp.status).toBe(200);
    // Code minted in DB.
    expect(state.activation_codes.size).toBe(1);
    const code = [...state.activation_codes.values()][0];
    expect(code.license_id).toBe(lid);
    expect(code.consumed_at).toBeNull();
    // Audit row.
    const audits = state.audit_log.filter(
      (a) => a.event_type === "account_delete_otp_sent"
    );
    expect(audits.length).toBe(1);
  });

  test("STEP 2: invalid code → 400", async () => {
    const state = emptyState();
    await seedLicensedAccount(state, "bob@dev.local");
    const resp = await handleAccountDelete(
      makeReq({ email: "bob@dev.local", code: "garbage" }),
      makeEnv(state), ctx
    );
    expect(resp.status).toBe(400);
  });

  test("STEP 2: valid code → license anonymised + audit", async () => {
    // Spec from delete.ts: status flips to 'deleted', email_hash
    // replaced by 'deleted-<ulid>' placeholder so the row can never be
    // found by email lookup again. Audit row records the action.
    const state = emptyState();
    const { eh, lid } = await seedLicensedAccount(state, "charlie@dev.local");
    // Step 1 first (mints a code).
    await handleAccountDelete(
      makeReq({ email: "charlie@dev.local" }), makeEnv(state), ctx
    );
    const otp = [...state.activation_codes.values()][0].code as string;
    // Step 2 — submit the code.
    const resp = await handleAccountDelete(
      makeReq({ email: "charlie@dev.local", code: otp }),
      makeEnv(state), ctx
    );
    expect(resp.status).toBe(200);
    const lic = state.licenses.get(lid)!;
    expect(lic.status).toBe("deleted");
    // email_hash replaced by placeholder, NOT the original.
    expect(lic.email_hash).not.toBe(eh);
    expect(lic.email_hash as string).toMatch(/^deleted-/);
    // Audit trail kept.
    const audits = state.audit_log.filter(
      (a) => a.event_type === "account_deleted"
    );
    expect(audits.length).toBe(1);
  });

  test("STEP 2: replay same code → 409 (one-shot)", async () => {
    const state = emptyState();
    await seedLicensedAccount(state, "dora@dev.local");
    await handleAccountDelete(
      makeReq({ email: "dora@dev.local" }), makeEnv(state), ctx
    );
    const otp = [...state.activation_codes.values()][0].code as string;
    // First consume.
    const r1 = await handleAccountDelete(
      makeReq({ email: "dora@dev.local", code: otp }), makeEnv(state), ctx
    );
    expect(r1.status).toBe(200);
    // Replay attempt — but the email is now anonymised so findActiveLicenseByEmail
    // returns null → handler returns 400 "invalid code". Either way:
    // unauthorised replay must NOT succeed.
    const r2 = await handleAccountDelete(
      makeReq({ email: "dora@dev.local", code: otp }), makeEnv(state), ctx
    );
    expect([400, 409]).toContain(r2.status);
  });

  test("STEP 2: code from another account → 400 (cross-account check)", async () => {
    // User A's code MUST NOT delete User B's account, even if A also
    // submits B's email. Defence in depth — we enforce both auth check
    // (the email + code combo) and explicit license_id binding check.
    const state = emptyState();
    await seedLicensedAccount(state, "eve@dev.local");
    const { lid: lid_v } = await seedLicensedAccount(state, "victim@dev.local");
    // Eve gets HER OTP.
    await handleAccountDelete(
      makeReq({ email: "eve@dev.local" }), makeEnv(state), ctx
    );
    const eveOtp = [...state.activation_codes.values()][0].code as string;
    // Eve tries Eve's code with Victim's email.
    const resp = await handleAccountDelete(
      makeReq({ email: "victim@dev.local", code: eveOtp }),
      makeEnv(state), ctx
    );
    expect(resp.status).toBe(400);
    // Victim untouched.
    expect(state.licenses.get(lid_v)!.status).toBe("active");
  });
});
