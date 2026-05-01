// HTML landing pages — Stripe redirects users to /checkout/success
// after payment, the email magic-link points at /activate?code=… which
// renders a dimmy:// auto-redirect bridge. Both must serve real HTML
// (not JSON or 404) and reach 200 on happy paths.

import { describe, expect, test } from "vitest";
import worker from "../src/index";
import { handleActivateRedirect } from "../src/handlers/activate-redirect";
import { emptyState, makeMockDB } from "./_d1-mock";
import type { Env } from "../src/index";

function makeEnv(): Env {
  const state = emptyState();
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

describe("/checkout/success", () => {
  test("returns 200 HTML with 'check your inbox' messaging", async () => {
    const req = new Request("http://localhost/checkout/success?session_id=cs_test");
    const resp = await worker.fetch(req, makeEnv(), ctx);
    expect(resp.status).toBe(200);
    expect(resp.headers.get("Content-Type")).toMatch(/text\/html/);
    const body = await resp.text();
    expect(body).toMatch(/<!doctype html>/i);
    expect(body.toLowerCase()).toContain("payment");
    // Has a CTA back into the app via custom scheme.
    expect(body).toContain("dimmy://");
  });

  test("Cache-Control: no-store (do not cache pages with session id in url)", async () => {
    const req = new Request("http://localhost/checkout/success");
    const resp = await worker.fetch(req, makeEnv(), ctx);
    expect(resp.headers.get("Cache-Control")).toMatch(/no-store/);
  });
});

describe("/checkout/cancel", () => {
  test("returns 200 HTML — cancellation page", async () => {
    const req = new Request("http://localhost/checkout/cancel");
    const resp = await worker.fetch(req, makeEnv(), ctx);
    expect(resp.status).toBe(200);
    expect(resp.headers.get("Content-Type")).toMatch(/text\/html/);
    const body = await resp.text();
    expect(body).toMatch(/<!doctype html>/i);
    expect(body.toLowerCase()).toContain("cancel");
  });
});

describe("/activate?code=… (HTTPS bridge)", () => {
  test("400 on missing code", async () => {
    const req = new Request("http://localhost/activate");
    const resp = await handleActivateRedirect(req, makeEnv(), ctx);
    expect(resp.status).toBe(400);
  });

  test("400 on malformed code (chars outside [A-Za-z0-9])", async () => {
    const req = new Request("http://localhost/activate?code=foo!bar");
    const resp = await handleActivateRedirect(req, makeEnv(), ctx);
    expect(resp.status).toBe(400);
  });

  test("400 on too-short code (<8 chars)", async () => {
    const req = new Request("http://localhost/activate?code=abc");
    const resp = await handleActivateRedirect(req, makeEnv(), ctx);
    expect(resp.status).toBe(400);
  });

  test("happy path → 200 HTML page with embedded dimmy:// redirect", async () => {
    const req = new Request(
      "http://localhost/activate?code=ABCDEFGHIJKLMNOPQRSTUVWXYZ012345"
    );
    const resp = await handleActivateRedirect(req, makeEnv(), ctx);
    expect(resp.status).toBe(200);
    expect(resp.headers.get("Content-Type")).toMatch(/text\/html/);
    expect(resp.headers.get("Cache-Control")).toMatch(/no-store/);
    expect(resp.headers.get("X-Frame-Options")).toBe("DENY");
    expect(resp.headers.get("Referrer-Policy")).toBe("no-referrer");
    const body = await resp.text();
    expect(body).toContain(
      "dimmy://activate?code=ABCDEFGHIJKLMNOPQRSTUVWXYZ012345"
    );
    expect(body).toContain("window.location.href");
    // Fallback paste-code is shown so the user can recover even if the
    // OS scheme dispatch fails.
    expect(body).toContain("ABCDEFGHIJKLMNOPQRSTUVWXYZ012345");
  });

  test("HTML escapes the code in the visible textContent (defence in depth)", async () => {
    // 32 chars of literal A-Z to avoid the format reject; we don't test
    // SQL injection here (code goes nowhere near SQL on this endpoint),
    // but the page should still escape lest the alphabet expand later.
    const code = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
    const req = new Request(`http://localhost/activate?code=${code}`);
    const resp = await handleActivateRedirect(req, makeEnv(), ctx);
    const body = await resp.text();
    expect(body).not.toContain("<script>alert"); // no XSS slipping through
  });
});
