// Dimmy licensing — Cloudflare Worker entry point.
//
// Replaces core/src/license_server.rs (axum PoC) with the production
// runtime. Wire shapes are identical so the Rust client doesn't care
// which side it's talking to.

import { handleTrialStart } from "./handlers/trial";
import { handleActivate } from "./handlers/activate";
import { handleRefresh } from "./handlers/refresh";
import { handleStripeWebhook } from "./handlers/stripe";
import { handleStatusDebug } from "./handlers/status";
import { handleAccountDelete } from "./handlers/delete";

/// Bindings injected by Cloudflare. Names must match wrangler.toml.
export interface Env {
  DB: D1Database;

  // Public, set in [vars] in wrangler.toml.
  PUBLIC_URL: string;
  EMAIL_FROM: string;
  STRIPE_PRICE_ANNUAL: string;
  STRIPE_PRICE_3YEAR: string;

  // Secrets, set via `wrangler secret put NAME`.
  DIMMY_LICENSE_PRIVKEY: string; // base64url(32-byte ed25519 private)
  DIMMY_LICENSE_PUBKEY: string;  // base64url(32-byte ed25519 public)
  STRIPE_WEBHOOK_SECRET: string;
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
