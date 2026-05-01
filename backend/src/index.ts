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
        return await handleBillingPortal(req, env, ctx);
      }
      if (method === "POST" && path === "/api/checkout/create") {
        return await handleCheckoutCreate(req, env, ctx);
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
function checkoutLandingPage(success: boolean): Response {
  const title = success ? "Payment received — check your inbox" : "Payment cancelled";
  const heading = success
    ? "Thanks! Your payment is confirmed."
    : "No charge was made.";
  const body = success
    ? `We've emailed an activation link to the address you used at checkout.
       Click it from the device you want to license — Dimmy will open and
       activate automatically. The link is valid for 10 minutes.`
    : `You can return to Dimmy and try again anytime.`;
  const html = `<!doctype html>
<html lang="en"><head>
<meta charset="utf-8">
<title>${title}</title>
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
.cta:hover { opacity: 0.9; }
.muted { font-size: 12px; color: #888; margin-top: 24px; }
</style></head><body>
<h1>${heading}</h1>
<p>${body}</p>
<a class="cta" href="dimmy://license">Open Dimmy</a>
<p class="muted">You can close this tab.</p>
</body></html>`;
  return new Response(html, {
    status: 200,
    headers: { "Content-Type": "text/html; charset=utf-8", "Cache-Control": "no-store" },
  });
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
