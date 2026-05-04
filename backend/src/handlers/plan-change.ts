// POST /api/plan-change { token, new_tier }
//
// Switch an active subscription license between monthly ⇄ annual via the
// Stripe `subscriptions.update` API (proration handled by Stripe). NOT
// for first purchase (use /api/checkout/create) and NOT for lifetime
// (lifetime is a one-time transaction; the duplicate-purchase gate in
// stripe.ts handles the in-place upgrade from a sub to lifetime).
//
// Why this endpoint exists: before it, "Switch to Annual" while on
// Active{Monthly} routed through /api/checkout/create → Stripe Checkout
// → second subscription created → user charged immediately for the new
// sub → webhook fires → duplicate-purchase gate cancels the new sub →
// user keeps their monthly AND eats the annual charge (cancellation
// doesn't auto-refund the just-collected invoice). Plan-change-via-API
// avoids this by mutating the existing sub in place: same sub id, new
// price, prorated invoice issued automatically by Stripe.
//
// Auth: token required (we look up the user's license + sub from the
// token's `lid` claim). Anonymous callers can't change someone else's
// subscription.

import type { Env } from "../index";
import { json } from "../index";
import {
  audit,
  findLicenseById,
} from "../db";
import { verifyTokenWithPub, type Claims } from "../crypto";

type PaidSubTier = "monthly" | "annual";

function isSubTier(s: unknown): s is PaidSubTier {
  return s === "monthly" || s === "annual";
}

function priceIdFor(env: Env, tier: PaidSubTier): string | null {
  return tier === "monthly"
    ? (env.STRIPE_PRICE_MONTHLY || null)
    : (env.STRIPE_PRICE_ANNUAL || null);
}

export async function handlePlanChange(
  req: Request,
  env: Env,
  _ctx: ExecutionContext
): Promise<Response> {
  if (!env.STRIPE_SECRET_KEY) {
    return json({ error: "plan change not configured" }, 500);
  }

  let body: { token?: unknown; new_tier?: unknown };
  try {
    body = await req.json();
  } catch {
    return json({ error: "invalid JSON body" }, 400);
  }
  if (typeof body.token !== "string" || body.token.length === 0) {
    return json({ error: "token required" }, 400);
  }
  if (!isSubTier(body.new_tier)) {
    return json(
      { error: "new_tier must be 'monthly' or 'annual' (lifetime uses /api/checkout/create)" },
      400
    );
  }
  const newTier: PaidSubTier = body.new_tier;

  let claims: Claims;
  try {
    claims = await verifyTokenWithPub(body.token, env.DIMMY_LICENSE_PUBKEY);
  } catch (err) {
    const msg = err instanceof Error ? err.message : "unknown";
    return json({ error: `invalid token: ${msg}` }, 400);
  }

  const lic = await findLicenseById(env.DB, claims.lid);
  if (!lic) return json({ error: "license not found" }, 404);
  if (lic.status !== "active") return json({ error: "license not active" }, 409);
  if (lic.tier === newTier) {
    return json({ status: "no_change", tier: newTier });
  }
  if (lic.tier === "trial") {
    return json(
      { error: "trial users must use /api/checkout/create to start a paid subscription" },
      409
    );
  }
  if (lic.tier === "lifetime") {
    return json(
      { error: "lifetime licenses cannot downgrade to a subscription" },
      409
    );
  }
  if (!lic.stripe_subscription_id) {
    return json(
      { error: "license has no stripe subscription id — cannot change plan via API" },
      409
    );
  }

  const newPriceId = priceIdFor(env, newTier);
  if (!newPriceId) {
    return json({ error: `${newTier} price not configured server-side` }, 500);
  }

  // Stripe API needs the SubscriptionItem id (not just the sub id) to
  // change its price. Fetch the sub first to read items[0].id.
  const subResp = await fetch(
    `https://api.stripe.com/v1/subscriptions/${encodeURIComponent(lic.stripe_subscription_id)}`,
    {
      headers: { Authorization: `Bearer ${env.STRIPE_SECRET_KEY}` },
    }
  );
  if (!subResp.ok) {
    const t = await subResp.text();
    return json(
      { error: `stripe sub fetch: ${subResp.status} ${t.slice(0, 200)}` },
      502
    );
  }
  const sub = (await subResp.json()) as {
    items?: { data?: Array<{ id?: string }> };
  };
  const itemId = sub.items?.data?.[0]?.id;
  if (!itemId) {
    return json({ error: "stripe sub has no items[0].id — cannot change plan" }, 502);
  }

  // Apply the price change. proration_behavior=create_prorations means
  // Stripe issues a credit/debit invoice item reflecting the unused
  // time on the old plan plus the prorated cost of the new plan, all
  // settled on the next regular invoice (or immediately, depending on
  // collection method — defaults to charge_automatically). The user
  // sees ONE adjustment, not a fresh full-price charge.
  const updateParams = new URLSearchParams();
  updateParams.set(`items[0][id]`, itemId);
  updateParams.set(`items[0][price]`, newPriceId);
  updateParams.set("proration_behavior", "create_prorations");
  // Update the metadata.tier so future webhook events for this sub
  // resolve to the new tier even before the price IDs match.
  updateParams.set("metadata[tier]", newTier);

  const updateResp = await fetch(
    `https://api.stripe.com/v1/subscriptions/${encodeURIComponent(lic.stripe_subscription_id)}`,
    {
      method: "POST",
      headers: {
        Authorization: `Bearer ${env.STRIPE_SECRET_KEY}`,
        "Content-Type": "application/x-www-form-urlencoded",
      },
      body: updateParams.toString(),
    }
  );
  if (!updateResp.ok) {
    const t = await updateResp.text();
    return json(
      { error: `stripe sub update: ${updateResp.status} ${t.slice(0, 200)}` },
      502
    );
  }

  // The customer.subscription.updated webhook will fire automatically —
  // its handler (handleSubscriptionUpdated in stripe.ts) detects the
  // new price → maps to new tier → updates licenses.tier in D1.
  // Until that webhook arrives the local row still says the old tier;
  // the next /api/refresh from the client picks up the new tier.

  const now = Math.floor(Date.now() / 1000);
  await audit(
    env.DB,
    {
      event_type: "plan_changed",
      email_hash: lic.email_hash,
      license_id: lic.license_id,
      details: {
        previous_tier: lic.tier,
        new_tier: newTier,
        stripe_subscription_id: lic.stripe_subscription_id,
        stripe_item_id: itemId,
      },
    },
    now
  );

  return json({
    status: "plan_changed",
    new_tier: newTier,
    note: "Stripe will issue a prorated invoice automatically. Refresh your license to see the new tier.",
  });
}
