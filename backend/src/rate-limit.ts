// Lightweight rate limiter for the staging/prod Worker.
//
// Goal: stop trivial abuse (someone scripting /api/trial/start with
// random emails to spam Resend, or hammering /api/checkout/create to
// burn Stripe rate budget). NOT a DDoS shield — that's Cloudflare WAF
// on the front of the zone.
//
// Strategy: fixed-window counter in D1, single UPSERT per call. The
// row keys on a "namespace:identity" pair: namespace identifies the
// endpoint, identity is whatever fingerprint we have (IP for anonymous
// endpoints, license_id for token-gated ones). When the window expires
// the row resets atomically inside the UPSERT (no race between two
// concurrent calls landing on different Worker instances — D1 is
// single-writer at the row level for ON CONFLICT).
//
// On 429 we set a Retry-After header so well-behaved clients back off
// without us having to teach them the limit values.

export interface RateLimitConfig {
  /// Logical bucket name — appears in the row key + 429 body.
  namespace: string;
  /// Max calls inside one window before we 429.
  limit: number;
  /// Window length in seconds. When a request lands and the row's
  /// window_start is older than now-period, the count resets.
  periodSecs: number;
}

export interface RateLimitOutcome {
  ok: boolean;
  /// Current count (post-increment on hit, or stale value on miss).
  count: number;
  /// Limit that applied. Always echoed for headers.
  limit: number;
  /// When the current window resets, in epoch seconds.
  resetAt: number;
}

/// Apply the limit for `identity` against `cfg`. Returns the outcome.
/// The row is incremented unconditionally — callers decide what to do
/// with the result.
export async function rateLimit(
  db: D1Database,
  identity: string,
  cfg: RateLimitConfig,
  nowSecs: number
): Promise<RateLimitOutcome> {
  // Defensive: an empty identity string would collapse all callers
  // onto the same bucket. Use a sentinel so the row at least exists,
  // and let the upstream caller decide whether that's OK.
  const safeIdentity = identity && identity.length > 0 ? identity : "_anon";
  const bucket = `${cfg.namespace}:${safeIdentity}`;
  const windowCutoff = nowSecs - cfg.periodSecs;

  // Single UPSERT that resets the window or increments in place,
  // returning the new count. RETURNING is supported by D1's SQLite.
  const stmt = db
    .prepare(
      `INSERT INTO rate_limits (bucket, window_start, count)
       VALUES (?1, ?2, 1)
       ON CONFLICT(bucket) DO UPDATE SET
         count = CASE
           WHEN rate_limits.window_start < ?3 THEN 1
           ELSE rate_limits.count + 1
         END,
         window_start = CASE
           WHEN rate_limits.window_start < ?3 THEN ?2
           ELSE rate_limits.window_start
         END
       RETURNING count, window_start`
    )
    .bind(bucket, nowSecs, windowCutoff);

  const row = (await stmt.first<{ count: number; window_start: number }>()) ?? {
    count: 1,
    window_start: nowSecs,
  };
  const resetAt = row.window_start + cfg.periodSecs;
  return {
    ok: row.count <= cfg.limit,
    count: row.count,
    limit: cfg.limit,
    resetAt,
  };
}

/// Build a 429 Response with the appropriate headers + JSON body.
/// Consistent with the rest of the API which always replies JSON.
export function rateLimitedResponse(
  outcome: RateLimitOutcome,
  nowSecs: number
): Response {
  const retryAfter = Math.max(1, outcome.resetAt - nowSecs);
  return new Response(
    JSON.stringify({
      error: "rate_limited",
      limit: outcome.limit,
      reset_at: outcome.resetAt,
      retry_after_secs: retryAfter,
    }),
    {
      status: 429,
      headers: {
        "Content-Type": "application/json",
        "Cache-Control": "no-store",
        "Retry-After": String(retryAfter),
        "X-RateLimit-Limit": String(outcome.limit),
        "X-RateLimit-Remaining": String(Math.max(0, outcome.limit - outcome.count)),
        "X-RateLimit-Reset": String(outcome.resetAt),
      },
    }
  );
}

/// Pull the client IP from a Worker request. CF-Connecting-IP is set by
/// the Cloudflare edge and trustworthy on a Worker. x-forwarded-for is
/// only consulted as a fallback for local `wrangler dev` where the CF
/// header isn't present.
export function clientIp(req: Request): string {
  const cf = req.headers.get("CF-Connecting-IP");
  if (cf && cf.length > 0) return cf;
  const xff = req.headers.get("x-forwarded-for");
  if (xff && xff.length > 0) return xff.split(",")[0]!.trim();
  return "_unknown";
}

/// Rate-limit configs by endpoint. Tuned for "stop a script", not DDoS.
/// Numbers picked from the smell test:
///   • trial:        humans don't restart their trial 5x/day
///   • checkout:     a real user clicks Buy maybe 1-2x within an hour
///   • plan-change:  near-zero per real account; 5/h is a generous slack
///   • billing:      legit "Manage subscription" maybe 3x/h max
export const RL = {
  trial: { namespace: "trial", limit: 5, periodSecs: 86_400 } as const,
  checkout: { namespace: "checkout", limit: 10, periodSecs: 3_600 } as const,
  planChange: { namespace: "plan_change", limit: 5, periodSecs: 3_600 } as const,
  billingPortal: { namespace: "billing_portal", limit: 10, periodSecs: 3_600 } as const,
};
