import { afterAll, beforeAll, describe, expect, test, vi } from "vitest";
import { verifyStripeSignature } from "../src/handlers/stripe";

const SECRET = "whsec_test_supersecret_value";
const BODY = JSON.stringify({ id: "evt_1", type: "checkout.session.completed" });

// Helper: build a valid Stripe-Signature header for a given timestamp.
async function buildSignature(
  body: string,
  secret: string,
  timestamp: number
): Promise<string> {
  const signingInput = `${timestamp}.${body}`;
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
      new TextEncoder().encode(signingInput)
    )
  );
  const macHex = [...macBytes]
    .map((b) => b.toString(16).padStart(2, "0"))
    .join("");
  return `t=${timestamp},v1=${macHex}`;
}

describe("verifyStripeSignature", () => {
  beforeAll(() => {
    // Pin Date.now so tolerance window is deterministic.
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-05-01T12:00:00Z"));
  });
  afterAll(() => {
    vi.useRealTimers();
  });

  test("accepts a freshly-signed valid signature", async () => {
    const ts = Math.floor(Date.now() / 1000);
    const header = await buildSignature(BODY, SECRET, ts);
    expect(await verifyStripeSignature(BODY, header, SECRET)).toBe(true);
  });

  test("rejects signature outside tolerance (>5min old)", async () => {
    const tooOld = Math.floor(Date.now() / 1000) - 600; // 10 min ago
    const header = await buildSignature(BODY, SECRET, tooOld);
    expect(await verifyStripeSignature(BODY, header, SECRET)).toBe(false);
  });

  test("rejects signature with future timestamp far ahead", async () => {
    const tooFuture = Math.floor(Date.now() / 1000) + 600;
    const header = await buildSignature(BODY, SECRET, tooFuture);
    expect(await verifyStripeSignature(BODY, header, SECRET)).toBe(false);
  });

  test("rejects header with wrong secret", async () => {
    const ts = Math.floor(Date.now() / 1000);
    const header = await buildSignature(BODY, "wrong_secret", ts);
    expect(await verifyStripeSignature(BODY, header, SECRET)).toBe(false);
  });

  test("rejects when body is mutated post-signature", async () => {
    const ts = Math.floor(Date.now() / 1000);
    const header = await buildSignature(BODY, SECRET, ts);
    expect(
      await verifyStripeSignature(BODY + "x", header, SECRET)
    ).toBe(false);
  });

  test("accepts when one of multiple v1 sigs matches (key rotation)", async () => {
    const ts = Math.floor(Date.now() / 1000);
    const goodSig = (await buildSignature(BODY, SECRET, ts)).split(",v1=")[1];
    const badSig = "00".repeat(32);
    const header = `t=${ts},v1=${badSig},v1=${goodSig}`;
    expect(await verifyStripeSignature(BODY, header, SECRET)).toBe(true);
  });

  test("rejects malformed header", async () => {
    expect(await verifyStripeSignature(BODY, "garbage", SECRET)).toBe(false);
    expect(await verifyStripeSignature(BODY, "t=123", SECRET)).toBe(false);
    expect(await verifyStripeSignature(BODY, "v1=abc", SECRET)).toBe(false);
  });
});
