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
import { verifyTokenWithPub, emailHash, type Claims } from "../crypto";
import { findLicenseById, findActiveLicenseByEmail } from "../db";

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

  let body: { tier?: unknown; token?: unknown; email?: unknown; return_url?: unknown };
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

  // Pre-checkout gate. Two sources of identity, in priority order:
  //
  //   1. Token (logged-in flow, claims.lid → DB lookup)
  //   2. Email body field (post-sign-out flow — UI prompts for email
  //      and passes it here so we can lookup by email_hash before
  //      letting Stripe charge)
  //
  // Either source resolves to a license row; if it's an active paid
  // license that conflicts with the requested tier, we 409 BEFORE
  // creating a Stripe Checkout. This is the radical fix to the
  // sign-out + re-buy bug witnessed live 2026-05-04: previously the
  // anonymous Checkout went through, the webhook gate refunded after
  // the fact, but the user briefly saw a charge on their card.
  //
  // Reject matrix:
  //   • already on lifetime          → 409 (ceiling)
  //   • already on monthly/annual + buy m|a → 409 (use plan-change)
  //   • already on monthly/annual + buy lifetime → PASS (sub→lifetime upgrade)
  //   • trial / no license / expired → PASS (legitimate fresh path)
  //
  // Stripe Customer dedup: if email is provided we pass it as
  // customer_email to Checkout. Stripe matches against existing
  // customer with same email; otherwise creates a new one. This
  // prevents the "two customers per email" pattern that caused
  // orphan refunds last cycle.
  let emailHashCarry: string | null = null;
  let stripeCustomerEmail: string | null = null;
  let existingActiveLicense: Awaited<ReturnType<typeof findLicenseById>> = null;

  // Source 1: token (preferred — proves the caller has the license).
  if (typeof body.token === "string" && body.token.length > 0) {
    try {
      const claims: Claims = await verifyTokenWithPub(
        body.token,
        env.DIMMY_LICENSE_PUBKEY
      );
      emailHashCarry = claims.eh;
      existingActiveLicense = await findLicenseById(env.DB, claims.lid);
    } catch {
      // Invalid token = silent fall-through; try email next.
      emailHashCarry = null;
    }
  }
  // Source 2: email body field (post-sign-out / first-purchase flow).
  // Only consulted if token didn't resolve a license — token is
  // authoritative when present and valid.
  if (!existingActiveLicense && typeof body.email === "string" && body.email.length > 0) {
    const e = body.email.trim().toLowerCase();
    if (e.includes("@") && e.length < 254) {
      stripeCustomerEmail = e;
      const eh = await emailHash(e);
      emailHashCarry = emailHashCarry ?? eh;
      existingActiveLicense = await findActiveLicenseByEmail(env.DB, eh);
    }
  }

  // Apply the gate against whatever license we resolved.
  if (existingActiveLicense && existingActiveLicense.status === "active") {
    const lic = existingActiveLicense;
    if (lic.tier === "lifetime") {
      return json(
        {
          error: "already on lifetime — no further purchase needed",
          current_tier: "lifetime",
          requested_tier: tier,
        },
        409
      );
    }
    if (
      (lic.tier === "monthly" || lic.tier === "annual") &&
      (tier === "monthly" || tier === "annual")
    ) {
      return json(
        {
          error:
            "already on a paid subscription — use plan-change to switch monthly⇄annual, or activate this device with the magic-link flow",
          current_tier: lic.tier,
          requested_tier: tier,
        },
        409
      );
    }
    // monthly|annual + buy lifetime falls through (sub→lifetime upgrade
    // is legitimate; the webhook gate will mutate the license in place).
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
  if (stripeCustomerEmail) {
    // Pre-populate Stripe Checkout's email field. Stripe will dedupe
    // against an existing Customer object with the same email rather
    // than creating a fresh one — fixes the legacy "two customers per
    // email" pattern that caused orphan charges before this change.
    params.append("customer_email", stripeCustomerEmail);
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
