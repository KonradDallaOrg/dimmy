// POST /api/checkout/create { tier, token?, return_url? }
//
// Mints a Stripe Checkout Session for the requested paid tier and
// returns its URL. The client (Settings → License → Buy / Upgrade)
// opens the URL in the system browser; user pays; Stripe fires the
// `checkout.session.completed` webhook → handlers/stripe.ts creates
// the license + emails the magic link.
//
// Three flows we support:
//   1. New user, no trial (NotFound):        no token sent, fresh purchase
//   2. Trial user upgrading to Pro:          token sent → email_hash carried
//      across as `client_reference_id` so the webhook can attach the new
//      paid license to the same email
//   3. Expired user re-buying:               same as #1, the old license stays
//      in DB as historical record but the new one supersedes it
//
// Auth: NONE for case #1 (anyone can buy), token verification for #2 so
// we can carry email_hash. We don't enforce token presence — case #1 is
// the explicit "no license yet" path.

import type { Env } from "../index";
import { json } from "../index";
import { verifyTokenWithPub, type Claims } from "../crypto";

type PaidTier = "monthly" | "annual" | "lifetime";

function priceForTier(env: Env, tier: PaidTier): string | null {
  switch (tier) {
    case "monthly":  return env.STRIPE_PRICE_MONTHLY  || null;
    case "annual":   return env.STRIPE_PRICE_ANNUAL   || null;
    case "lifetime": return env.STRIPE_PRICE_LIFETIME || null;
  }
}

function isPaidTier(s: unknown): s is PaidTier {
  return s === "monthly" || s === "annual" || s === "lifetime";
}

export async function handleCheckoutCreate(
  req: Request,
  env: Env,
  _ctx: ExecutionContext
): Promise<Response> {
  if (!env.STRIPE_SECRET_KEY) {
    return json({ error: "checkout not configured" }, 500);
  }

  let body: { tier?: unknown; token?: unknown; return_url?: unknown };
  try {
    body = await req.json();
  } catch {
    return json({ error: "invalid JSON body" }, 400);
  }
  if (!isPaidTier(body.tier)) {
    return json({ error: "tier must be monthly, annual, or lifetime" }, 400);
  }
  const tier: PaidTier = body.tier;
  const priceId = priceForTier(env, tier);
  if (!priceId) {
    return json({ error: `${tier} price not configured server-side` }, 500);
  }

  // Optional token — if present we verify it and pass email_hash to
  // Stripe as `client_reference_id` so the webhook can link the
  // purchase to the trial license. Invalid token = silent fall-through
  // to the anonymous-purchase path; never blocks the buy intent.
  let emailHashCarry: string | null = null;
  if (typeof body.token === "string" && body.token.length > 0) {
    try {
      const claims: Claims = await verifyTokenWithPub(
        body.token,
        env.DIMMY_LICENSE_PUBKEY
      );
      emailHashCarry = claims.eh;
    } catch {
      emailHashCarry = null;
    }
  }

  // return_url: where Stripe sends the user after success/cancel.
  // Sanitised — only https/dimmy schemes accepted, else fall back to
  // the public landing page. dimmy://license deep-links back into the
  // app's License page so the user lands on a familiar surface.
  const fallbackSuccess = `${env.PUBLIC_URL}/checkout/success?session_id={CHECKOUT_SESSION_ID}`;
  const fallbackCancel  = `${env.PUBLIC_URL}/checkout/cancel`;
  let successUrl = fallbackSuccess;
  let cancelUrl  = fallbackCancel;
  if (typeof body.return_url === "string" && body.return_url.length < 2048) {
    if (
      body.return_url.startsWith("https://") ||
      body.return_url.startsWith("dimmy://")
    ) {
      // Caller's URL replaces both — the app handles its own
      // success/cancel branching once it deep-links back.
      successUrl = body.return_url;
      cancelUrl  = body.return_url;
    }
  }

  // Subscription vs one-time mode — `lifetime` is `mode=payment`,
  // monthly/annual are `mode=subscription`. Stripe rejects mismatched
  // mode/price combinations server-side, so we map explicitly here.
  const mode = tier === "lifetime" ? "payment" : "subscription";

  const params = new URLSearchParams({
    mode,
    "line_items[0][price]":    priceId,
    "line_items[0][quantity]": "1",
    success_url:               successUrl,
    cancel_url:                cancelUrl,
    // Always collect billing details so Stripe Tax has everything it
    // needs (we leave Tax behaviour itself configured at the account
    // level — Tax must be enabled in the dashboard).
    "billing_address_collection": "required",
    // metadata.tier is the canonical way the webhook handler resolves
    // which plan was bought (line_items aren't included by default in
    // checkout.session.completed payloads — we'd have to retrieve+expand).
    "metadata[tier]": tier,
  });
  if (emailHashCarry) {
    params.append("client_reference_id", emailHashCarry);
  }

  const resp = await fetch("https://api.stripe.com/v1/checkout/sessions", {
    method: "POST",
    headers: {
      Authorization: `Bearer ${env.STRIPE_SECRET_KEY}`,
      "Content-Type": "application/x-www-form-urlencoded",
    },
    body: params.toString(),
  });
  if (!resp.ok) {
    const text = await resp.text();
    return json(
      { error: `stripe checkout: ${resp.status} ${text.slice(0, 200)}` },
      502
    );
  }
  const session = (await resp.json()) as { url?: string };
  if (!session.url) {
    return json({ error: "stripe checkout: missing url in response" }, 502);
  }
  return json({ url: session.url, tier });
}
