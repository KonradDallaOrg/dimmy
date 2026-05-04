// Stripe Test Clock simulation of the monthly→annual plan-change flow.
//
// Reproduces the exact path our webhook + plan-change endpoint hit:
//   1. Create a test clock (frozen time)
//   2. Create customer attached to the clock
//   3. Create monthly subscription
//   4. Update sub price to annual + proration_behavior=create_prorations
//      (matches handlers/plan-change.ts)
//   5. Inspect current_period_end (should still be ~30 days from now —
//      this is the '31 giorni alla scadenza' bug witness)
//   6. ADVANCE the clock past period_end → Stripe renews:
//      - issues annual invoice with prorated credit
//      - new current_period_end = +1 year
//   7. Confirm post-renewal period_end is +1 year.
//
// Run:
//   STRIPE_KEY=sk_test_... node scripts/stripe-time-travel-test.mjs
// (the env var is auto-loaded from .env when STRIPE_KEY missing)
//
// Cleanup at the end deletes the test clock — Stripe also deletes the
// customer + sub + invoices that hang off it, so you don't leave test
// artifacts lying around.

import { readFileSync } from "node:fs";

const STRIPE_API = "https://api.stripe.com/v1";

function loadEnvKey() {
  if (process.env.STRIPE_KEY) return process.env.STRIPE_KEY;
  try {
    const env = readFileSync(".env", "utf8");
    const m = env.match(/^DIMMY_STRIPE_TEST_KEY=(.+)$/m);
    if (m) return m[1].trim();
  } catch {}
  throw new Error("STRIPE_KEY env var or DIMMY_STRIPE_TEST_KEY in .env required");
}

const KEY = loadEnvKey();

async function stripe(method, path, body = null) {
  const opts = {
    method,
    headers: {
      Authorization: `Bearer ${KEY}`,
    },
  };
  if (body) {
    opts.headers["Content-Type"] = "application/x-www-form-urlencoded";
    const params = new URLSearchParams();
    for (const [k, v] of Object.entries(body)) {
      if (Array.isArray(v)) v.forEach((vv) => params.append(`${k}[]`, vv));
      else params.append(k, String(v));
    }
    opts.body = params.toString();
  }
  const r = await fetch(`${STRIPE_API}${path}`, opts);
  const text = await r.text();
  if (!r.ok) throw new Error(`Stripe ${method} ${path} ${r.status}: ${text.slice(0, 300)}`);
  return JSON.parse(text);
}

const C = {
  ok: "\x1b[32m✓\x1b[0m",
  info: "\x1b[36m▶\x1b[0m",
  warn: "\x1b[33m⚠\x1b[0m",
  dim: "\x1b[90m",
  reset: "\x1b[0m",
};

function fmt(ts) {
  if (!ts) return "?";
  return new Date(ts * 1000).toISOString().replace("T", " ").slice(0, 19);
}

const PRICE_MONTHLY = "price_1TSKE8HxRNDPFvsZegNx8slR";
const PRICE_ANNUAL = "price_1TSKE9HxRNDPFvsZv4T1Ampf";

async function main() {
  const now = Math.floor(Date.now() / 1000);
  console.log(`${C.info} Starting Stripe test-clock simulation at ${fmt(now)}`);

  // 1. Create a test clock frozen at `now`.
  const clock = await stripe("POST", "/test_helpers/test_clocks", {
    frozen_time: now,
    name: "monthly-to-annual-rollover",
  });
  console.log(`${C.ok} test_clock ${clock.id} frozen at ${fmt(clock.frozen_time)}`);

  // 2. Customer attached to the clock.
  const cust = await stripe("POST", "/customers", {
    email: "timetravel@example.com",
    test_clock: clock.id,
    name: "Time Travel Tester",
    "metadata[purpose]": "monthly→annual rollover simulation",
  });
  console.log(`${C.ok} customer ${cust.id} on test_clock`);

  // 3. Add a payment method (Stripe TEST card token) and attach it.
  const pm = await stripe("POST", "/payment_methods", {
    type: "card",
    "card[token]": "tok_visa",
  });
  await stripe("POST", `/payment_methods/${pm.id}/attach`, { customer: cust.id });
  await stripe("POST", `/customers/${cust.id}`, {
    "invoice_settings[default_payment_method]": pm.id,
  });
  console.log(`${C.ok} payment method ${pm.id} attached as default`);

  // 4. Create monthly subscription. Stripe charges immediately for the
  // first cycle; current_period_end = now + ~30 days.
  let sub = await stripe("POST", "/subscriptions", {
    customer: cust.id,
    "items[0][price]": PRICE_MONTHLY,
    "metadata[tier]": "monthly",
  });
  // Modern Stripe API moved current_period_{start,end} from the
  // subscription object onto each subscription_item. Read them from
  // items.data[0] with a top-level fallback for older API versions.
  const periodStart = (s) =>
    s.items?.data?.[0]?.current_period_start ?? s.current_period_start;
  const periodEnd = (s) =>
    s.items?.data?.[0]?.current_period_end ?? s.current_period_end;
  console.log(`${C.ok} sub ${sub.id} created tier=monthly status=${sub.status}`);
  console.log(`  ${C.dim}current_period_start = ${fmt(periodStart(sub))}${C.reset}`);
  console.log(`  ${C.dim}current_period_end   = ${fmt(periodEnd(sub))}${C.reset}`);

  // 5. Plan change: update price to annual with proration_behavior=create_prorations.
  // EXACTLY what handlers/plan-change.ts does today.
  const itemId = sub.items.data[0].id;
  sub = await stripe("POST", `/subscriptions/${sub.id}`, {
    "items[0][id]": itemId,
    "items[0][price]": PRICE_ANNUAL,
    proration_behavior: "create_prorations",
    "metadata[tier]": "annual",
  });
  console.log(`${C.ok} sub mutated → price=annual, proration_behavior=create_prorations`);
  console.log(`  ${C.dim}current_period_start = ${fmt(periodStart(sub))} ← still monthly!${C.reset}`);
  console.log(`  ${C.dim}current_period_end   = ${fmt(periodEnd(sub))} ← still monthly cycle${C.reset}`);
  console.log(`  ${C.warn} THIS IS THE '31 GIORNI' BUG: tier=annual but period_end is the monthly's.`);

  // 6. Advance the clock past the monthly period_end. Stripe will:
  // - finalize the proration invoice
  // - charge the annual amount minus the credit
  // - set current_period_end to +1 year from the new cycle start
  const advanceTo = periodEnd(sub) + 60; // 1 minute past the end
  console.log(`\n${C.info} Advancing clock to ${fmt(advanceTo)} (past monthly period_end)…`);
  await stripe("POST", `/test_helpers/test_clocks/${clock.id}/advance`, {
    frozen_time: advanceTo,
  });
  // Poll until clock status flips to 'ready' (advance is async).
  let clockState;
  for (let i = 0; i < 30; i++) {
    clockState = await stripe("GET", `/test_helpers/test_clocks/${clock.id}`);
    if (clockState.status === "ready") break;
    await new Promise((r) => setTimeout(r, 1000));
    process.stdout.write(".");
  }
  console.log(`\n${C.ok} clock advanced (status=${clockState.status})`);

  // 7. Re-fetch the sub. Annual cycle should be in effect.
  sub = await stripe("GET", `/subscriptions/${sub.id}`);
  console.log(`${C.ok} sub after rollover:`);
  console.log(`  status               = ${sub.status}`);
  console.log(`  items[0].price       = ${sub.items.data[0].price.id} ${C.dim}(annual)${C.reset}`);
  console.log(`  current_period_start = ${fmt(periodStart(sub))}`);
  console.log(`  current_period_end   = ${fmt(periodEnd(sub))} ${C.dim}← now +1 year${C.reset}`);
  const yearAhead = periodEnd(sub) - now;
  console.log(`  → period_end is ${(yearAhead / 86400).toFixed(0)} days from the original purchase moment`);

  // 8. Latest invoice — should show the prorated annual charge.
  const invs = await stripe("GET", `/invoices?subscription=${sub.id}&limit=5`);
  console.log(`\n${C.info} Recent invoices for this sub:`);
  for (const inv of invs.data.reverse()) {
    console.log(
      `  ${inv.id} status=${inv.status} total=${(inv.total / 100).toFixed(2)} ${inv.currency.toUpperCase()} created=${fmt(inv.created)}`
    );
    for (const line of inv.lines.data) {
      const sign = line.amount < 0 ? "-" : "+";
      console.log(
        `    ${sign}${(Math.abs(line.amount) / 100).toFixed(2)} ${line.description ?? line.price?.id ?? "?"}`
      );
    }
  }

  // 9. Cleanup — deletes the clock and everything attached.
  console.log(`\n${C.info} Cleaning up test clock + dependent objects…`);
  await stripe("DELETE", `/test_helpers/test_clocks/${clock.id}`);
  console.log(`${C.ok} done. Customer ${cust.id} and sub ${sub.id} are gone.`);
}

main().catch((e) => {
  console.error(`\n\x1b[31mFAILED:\x1b[0m ${e.message}`);
  process.exit(1);
});
