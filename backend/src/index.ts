// Dimmy licensing — Cloudflare Worker entry point.
//
// Replaces core/src/license_server.rs (axum PoC) with the production
// runtime. Wire shapes are identical so the Rust client doesn't care
// which side it's talking to.

import { handleTrialStart } from "./handlers/trial";
import { handleActivate } from "./handlers/activate";
import { handleActivateRedirect } from "./handlers/activate-redirect";
import { handleRefresh } from "./handlers/refresh";
import { handleStripeWebhook } from "./handlers/stripe";
import { handleStatusDebug } from "./handlers/status";
import { handleAccountDelete } from "./handlers/delete";
import { handleDevicesList, handleDeviceDeactivate } from "./handlers/devices";
import { handleBillingPortal } from "./handlers/billing-portal";
import { handleCheckoutCreate } from "./handlers/checkout";
import { handlePlanChange } from "./handlers/plan-change";
import { RL, rateLimit, rateLimitedResponse, clientIp } from "./rate-limit";
import { verifyTokenWithPub } from "./crypto";

/// Bindings injected by Cloudflare. Names must match wrangler.toml.
export interface Env {
  DB: D1Database;

  // Public, set in [vars] in wrangler.toml.
  PUBLIC_URL: string;
  EMAIL_FROM: string;
  STRIPE_PRICE_MONTHLY: string;
  STRIPE_PRICE_ANNUAL: string;
  STRIPE_PRICE_LIFETIME: string;

  // Secrets, set via `wrangler secret put NAME`.
  DIMMY_LICENSE_PRIVKEY: string; // base64url(32-byte ed25519 private)
  DIMMY_LICENSE_PUBKEY: string;  // base64url(32-byte ed25519 public)
  STRIPE_WEBHOOK_SECRET: string;
  STRIPE_SECRET_KEY: string;     // sk_test_… or sk_live_…
  RESEND_API_KEY: string;
}

export default {
  async fetch(req: Request, env: Env, ctx: ExecutionContext): Promise<Response> {
    const url = new URL(req.url);
    const method = req.method.toUpperCase();
    const path = url.pathname;

    try {
      if (method === "GET" && path === "/api/health") {
        return json({ status: "ok" });
      }
      if (method === "POST" && path === "/api/trial/start") {
        const now = Math.floor(Date.now() / 1000);
        const r = await rateLimit(env.DB, clientIp(req), RL.trial, now);
        if (!r.ok) return rateLimitedResponse(r, now);
        return await handleTrialStart(req, env, ctx);
      }
      if (method === "GET" && path === "/api/activate") {
        return await handleActivate(req, env, ctx);
      }
      // Email-friendly HTTPS bridge to the dimmy:// scheme. Most email
      // clients strip custom schemes from links — this lets us send an
      // https://license.dimmy.app/activate?code=… URL that any email
      // client treats as clickable.
      if (method === "GET" && path === "/activate") {
        return await handleActivateRedirect(req, env, ctx);
      }
      if (method === "POST" && path === "/api/refresh") {
        return await handleRefresh(req, env, ctx);
      }
      if (method === "POST" && path === "/api/stripe/webhook") {
        return await handleStripeWebhook(req, env, ctx);
      }
      if (method === "GET" && path === "/api/license/status") {
        return await handleStatusDebug(req, env, ctx);
      }
      if (method === "POST" && path === "/api/account/delete") {
        return await handleAccountDelete(req, env, ctx);
      }
      if (method === "POST" && path === "/api/devices/list") {
        return await handleDevicesList(req, env, ctx);
      }
      if (method === "POST" && path === "/api/devices/deactivate") {
        return await handleDeviceDeactivate(req, env, ctx);
      }
      if (method === "POST" && path === "/api/billing-portal") {
        const now = Math.floor(Date.now() / 1000);
        const id = (await tokenIdentity(req, env)) ?? clientIp(req);
        const r = await rateLimit(env.DB, id, RL.billingPortal, now);
        if (!r.ok) return rateLimitedResponse(r, now);
        return await handleBillingPortal(req, env, ctx);
      }
      if (method === "POST" && path === "/api/checkout/create") {
        const now = Math.floor(Date.now() / 1000);
        const r = await rateLimit(env.DB, clientIp(req), RL.checkout, now);
        if (!r.ok) return rateLimitedResponse(r, now);
        return await handleCheckoutCreate(req, env, ctx);
      }
      if (method === "POST" && path === "/api/plan-change") {
        const now = Math.floor(Date.now() / 1000);
        const id = (await tokenIdentity(req, env)) ?? clientIp(req);
        const r = await rateLimit(env.DB, id, RL.planChange, now);
        if (!r.ok) return rateLimitedResponse(r, now);
        return await handlePlanChange(req, env, ctx);
      }
      // Stripe Checkout success / cancel landing pages. Stripe redirects
      // the user here after the hosted Checkout. The webhook fires async
      // in parallel — we don't depend on this hit for license creation;
      // it's purely UX confirmation that lets the user open Dimmy back.
      if (method === "GET" && (path === "/checkout/success" || path === "/checkout/cancel")) {
        return checkoutLandingPage(path === "/checkout/success");
      }
      return json({ error: "not found" }, 404);
    } catch (err) {
      // Last-ditch: never let an unhandled error bubble to a 500 with
      // stacktrace in body. Log it (Workers tail) and return a clean
      // envelope.
      console.error("[unhandled]", err);
      const msg = err instanceof Error ? err.message : "internal error";
      return json({ error: msg }, 500);
    }
  },
} satisfies ExportedHandler<Env>;

/// Minimal post-checkout landing page. Stripe-side payment is settled
/// by the time the user lands here; the webhook creates the license
/// in parallel + Resend sends the activation magic-link email.
//
// On success: do NOT promote "Open Dimmy" as the primary CTA — at this
// instant the app has nothing useful to show until the magic-link click
// arrives via email. Lead with "check your inbox" + webmail shortcuts;
// "Open Dimmy" is a tertiary link for users who already activated this
// device earlier and just want to return.
function checkoutLandingPage(success: boolean): Response {
  if (!success) {
    return new Response(cancelHtml(), {
      status: 200,
      headers: { "Content-Type": "text/html; charset=utf-8", "Cache-Control": "no-store" },
    });
  }
  const html = `<!doctype html>
<html lang="en"><head>
<meta charset="utf-8">
<title>Payment confirmed — check your inbox</title>
<meta name="viewport" content="width=device-width, initial-scale=1">
<style>
:root { color-scheme: light dark; }
body { font: 15px/1.55 -apple-system, BlinkMacSystemFont, "Segoe UI", system-ui, sans-serif;
       max-width: 520px; margin: 8vh auto; padding: 0 24px; }
h1 { font-size: 22px; margin: 0 0 8px; }
.lede { color: #555; }
.box { margin: 24px 0; padding: 16px 18px; border: 1px solid #e2e8f0;
       border-radius: 10px; background: #f8fafc; }
@media (prefers-color-scheme: dark) {
  .box { background: #1e293b; border-color: #334155; }
  .lede, .muted { color: #94a3b8; }
}
.box strong { display: block; font-size: 14px; margin-bottom: 6px; }
.webmail { display: flex; flex-wrap: wrap; gap: 8px; margin-top: 8px; }
.webmail a { display: inline-block; padding: 8px 14px; border-radius: 8px;
             background: #1a73e8; color: white; text-decoration: none;
             font-weight: 600; font-size: 14px; }
.webmail a:hover { opacity: 0.9; }
.webmail a.alt { background: #475569; }
.tertiary { font-size: 13px; margin-top: 28px; }
.tertiary a { color: #64748b; }
.muted { font-size: 12px; color: #888; margin-top: 16px; }
</style></head><body>
<h1>Payment confirmed.</h1>
<p class="lede">We've sent an activation link to the email you used at checkout.
Open it from the device you want to license — Dimmy activates automatically.
The link is valid for 10 minutes.</p>

<div class="box">
  <strong>Open your inbox</strong>
  <div class="webmail">
    <a href="https://mail.google.com/mail/u/0/#search/from%3Astaging%40dimmy.app+OR+from%3Ahello%40dimmy.app+newer_than%3A1h" target="_blank" rel="noopener">Gmail</a>
    <a href="https://outlook.live.com/mail/0/inbox" target="_blank" rel="noopener" class="alt">Outlook</a>
    <a href="https://www.icloud.com/mail" target="_blank" rel="noopener" class="alt">iCloud</a>
    <a href="https://mail.yahoo.com/" target="_blank" rel="noopener" class="alt">Yahoo</a>
  </div>
</div>

<p class="lede"><strong>Don't see it?</strong> Check spam, or wait a minute —
delivery is usually under 30s but occasionally drifts.</p>

<p class="tertiary">Already activated on this device? <a href="dimmy://license">Open Dimmy →</a></p>
<p class="muted">You can close this tab once you've clicked the link.</p>
</body></html>`;
  return new Response(html, {
    status: 200,
    headers: { "Content-Type": "text/html; charset=utf-8", "Cache-Control": "no-store" },
  });
}

function cancelHtml(): string {
  return `<!doctype html>
<html lang="en"><head>
<meta charset="utf-8">
<title>Payment cancelled</title>
<meta name="viewport" content="width=device-width, initial-scale=1">
<style>
:root { color-scheme: light dark; }
body { font: 15px/1.5 -apple-system, BlinkMacSystemFont, "Segoe UI", system-ui, sans-serif;
       max-width: 480px; margin: 8vh auto; padding: 0 24px; }
h1 { font-size: 22px; margin: 0 0 8px; }
p  { color: #555; }
.cta { display: inline-block; margin-top: 18px; padding: 10px 18px;
       background: #1a73e8; color: white; border-radius: 8px;
       text-decoration: none; font-weight: 600; }
.muted { font-size: 12px; color: #888; margin-top: 24px; }
</style></head><body>
<h1>No charge was made.</h1>
<p>You can return to Dimmy and try again anytime.</p>
<a class="cta" href="dimmy://license">Open Dimmy</a>
<p class="muted">You can close this tab.</p>
</body></html>`;
}

/// Best-effort extraction of the license_id from a token-bearing POST.
/// Used by the rate limiter to scope per-license rather than per-IP for
/// endpoints that already require a token (plan-change, billing-portal).
/// On any error returns null — the caller falls back to clientIp().
///
/// Note: this clones the request because verifyToken consumes the body.
/// The downstream handler will re-read the body from the original
/// req.json(), which is a no-op in Workers (Request body is replayable
/// when cloned this way).
async function tokenIdentity(req: Request, env: Env): Promise<string | null> {
  try {
    const clone = req.clone();
    const body = (await clone.json()) as { token?: unknown };
    if (typeof body.token !== "string" || body.token.length === 0) return null;
    const claims = await verifyTokenWithPub(body.token, env.DIMMY_LICENSE_PUBKEY);
    return claims.lid ?? null;
  } catch {
    return null;
  }
}

/// JSON response helper. Always sets the `application/json` Content-Type
/// and a no-cache header so middleboxes don't cache license responses.
export function json(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: {
      "Content-Type": "application/json",
      "Cache-Control": "no-store",
    },
  });
}
