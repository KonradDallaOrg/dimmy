// POST /api/billing-portal { token }
//
// Returns a short-lived Stripe Customer Portal URL for the license
// behind the supplied token. The portal lets the customer:
//   - update their card / payment method
//   - cancel or reactivate a subscription
//   - download invoices / receipts
//   - change plan (if multiple prices configured)
//
// Auth: same shape as /api/devices/* — caller proves identity by
// sending its current Ed25519-signed token. We verify the signature
// server-side, look up the license, and only mint a portal session
// for licenses that have a `stripe_customer_id` (i.e. they came from
// a real Stripe checkout, not a /api/trial/start).
//
// The Stripe Portal URL is valid for ~5 min and single-tab; the
// client should open it in the system browser immediately.

import type { Env } from "../index";
import { json } from "../index";
import { findLicenseById } from "../db";
import { verifyTokenWithPub, type Claims } from "../crypto";

export async function handleBillingPortal(
  req: Request,
  env: Env,
  _ctx: ExecutionContext
): Promise<Response> {
  if (!env.STRIPE_SECRET_KEY) {
    return json({ error: "billing portal not configured" }, 500);
  }

  let body: { token?: unknown; return_url?: unknown };
  try {
    body = await req.json();
  } catch {
    return json({ error: "invalid JSON body" }, 400);
  }
  const token = typeof body.token === "string" ? body.token : "";
  if (!token) return json({ error: "token required" }, 400);

  let claims: Claims;
  try {
    claims = await verifyTokenWithPub(token, env.DIMMY_LICENSE_PUBKEY);
  } catch (err) {
    const msg = err instanceof Error ? err.message : "unknown";
    return json({ error: `invalid token: ${msg}` }, 400);
  }

  const lic = await findLicenseById(env.DB, claims.lid);
  if (!lic) return json({ error: "license not found" }, 404);
  if (lic.status === "deleted") return json({ error: "license deleted" }, 409);
  if (!lic.stripe_customer_id) {
    // Trials and source-build licenses have no Stripe customer attached.
    return json(
      {
        error:
          "this license has no Stripe billing — only paid licenses can manage subscriptions",
      },
      409
    );
  }

  // Optional caller-provided return_url (e.g. dimmy://license to bring
  // the user back into the app once they're done in the portal). We
  // sanitise to https/dimmy schemes only — never leak users to a
  // third-party redirect from a Stripe-hosted page.
  let returnUrl = `${env.PUBLIC_URL}/portal-return`;
  if (typeof body.return_url === "string" && body.return_url.length < 2048) {
    if (
      body.return_url.startsWith("https://") ||
      body.return_url.startsWith("dimmy://")
    ) {
      returnUrl = body.return_url;
    }
  }

  // POST https://api.stripe.com/v1/billing_portal/sessions
  // Form-encoded (Stripe REST convention). Auth via Bearer secret key.
  const params = new URLSearchParams({
    customer: lic.stripe_customer_id,
    return_url: returnUrl,
  });
  const resp = await fetch("https://api.stripe.com/v1/billing_portal/sessions", {
    method: "POST",
    headers: {
      Authorization: `Bearer ${env.STRIPE_SECRET_KEY}`,
      "Content-Type": "application/x-www-form-urlencoded",
    },
    body: params.toString(),
  });
  if (!resp.ok) {
    const text = await resp.text();
    // Truncate to avoid leaking long Stripe error bodies that may
    // include request IDs the client doesn't need.
    return json(
      { error: `stripe portal: ${resp.status} ${text.slice(0, 200)}` },
      502
    );
  }
  const session = (await resp.json()) as { url?: string };
  if (!session.url) {
    return json({ error: "stripe portal: missing url in response" }, 502);
  }
  return json({ url: session.url });
}
