import { describe, expect, test } from "vitest";
import { rateLimit, rateLimitedResponse, rlConfigsFor } from "../src/rate-limit";
import { emptyState, makeMockDB } from "./_d1-mock";

const cfg = { namespace: "test", limit: 3, periodSecs: 60 } as const;

function db() {
  return makeMockDB(emptyState()) as unknown as D1Database;
}

describe("rateLimit", () => {
  test("allows up to limit then blocks", async () => {
    const d = db();
    const now = 1_000_000;
    for (let i = 1; i <= 3; i++) {
      const r = await rateLimit(d, "1.2.3.4", cfg, now);
      expect(r.ok).toBe(true);
      expect(r.count).toBe(i);
    }
    const blocked = await rateLimit(d, "1.2.3.4", cfg, now);
    expect(blocked.ok).toBe(false);
    expect(blocked.count).toBe(4);
    expect(blocked.limit).toBe(3);
  });

  test("resets when the window expires", async () => {
    const d = db();
    const t0 = 1_000_000;
    for (let i = 0; i < 3; i++) await rateLimit(d, "ip", cfg, t0);
    const blocked = await rateLimit(d, "ip", cfg, t0);
    expect(blocked.ok).toBe(false);

    // 61 seconds later → window resets, fresh budget.
    const fresh = await rateLimit(d, "ip", cfg, t0 + 61);
    expect(fresh.ok).toBe(true);
    expect(fresh.count).toBe(1);
  });

  test("isolates buckets per identity", async () => {
    const d = db();
    const now = 1_000_000;
    for (let i = 0; i < 3; i++) await rateLimit(d, "alice", cfg, now);
    // Alice exhausted, Bob still fresh.
    const bob = await rateLimit(d, "bob", cfg, now);
    expect(bob.ok).toBe(true);
    expect(bob.count).toBe(1);
  });

  test("isolates buckets per namespace", async () => {
    const d = db();
    const now = 1_000_000;
    for (let i = 0; i < 3; i++) await rateLimit(d, "ip", cfg, now);
    const blockedSame = await rateLimit(d, "ip", cfg, now);
    expect(blockedSame.ok).toBe(false);
    // Different namespace, same identity → fresh bucket.
    const otherNs = await rateLimit(d, "ip", { namespace: "other", limit: 3, periodSecs: 60 }, now);
    expect(otherNs.ok).toBe(true);
  });

  test("empty identity falls back to _anon (does not crash)", async () => {
    const d = db();
    const r = await rateLimit(d, "", cfg, 1_000_000);
    expect(r.ok).toBe(true);
  });

  test("resetAt = window_start + periodSecs", async () => {
    const d = db();
    const t0 = 1_000_000;
    const r = await rateLimit(d, "x", cfg, t0);
    expect(r.resetAt).toBe(t0 + 60);
  });
});

describe("rateLimitedResponse", () => {
  test("emits 429 with Retry-After + X-RateLimit headers", async () => {
    const outcome = { ok: false, count: 4, limit: 3, resetAt: 1_000_060 };
    const resp = rateLimitedResponse(outcome, 1_000_000);
    expect(resp.status).toBe(429);
    expect(resp.headers.get("Retry-After")).toBe("60");
    expect(resp.headers.get("X-RateLimit-Limit")).toBe("3");
    expect(resp.headers.get("X-RateLimit-Remaining")).toBe("0");
    expect(resp.headers.get("X-RateLimit-Reset")).toBe("1000060");
    const body = (await resp.json()) as { error: string; retry_after_secs: number };
    expect(body.error).toBe("rate_limited");
    expect(body.retry_after_secs).toBe(60);
  });

  test("Retry-After is clamped to a minimum of 1s", async () => {
    const outcome = { ok: false, count: 999, limit: 3, resetAt: 999_999 };
    const resp = rateLimitedResponse(outcome, 1_000_000);
    expect(resp.headers.get("Retry-After")).toBe("1");
  });
});

describe("rlConfigsFor", () => {
  test("prod URL → tight prod limits (5/day trial, 10/h checkout)", () => {
    const cfg = rlConfigsFor("https://license.dimmy.app");
    expect(cfg.trial.limit).toBe(5);
    expect(cfg.checkout.limit).toBe(10);
    expect(cfg.planChange.limit).toBe(5);
    expect(cfg.billingPortal.limit).toBe(10);
  });

  test("staging URL → loose staging limits (50/day trial, 100/h checkout)", () => {
    const cfg = rlConfigsFor("https://license-staging.dimmy.app");
    expect(cfg.trial.limit).toBe(50);
    expect(cfg.checkout.limit).toBe(100);
    expect(cfg.planChange.limit).toBe(50);
    expect(cfg.billingPortal.limit).toBe(100);
  });

  test("workers.dev fallback URL also counts as staging", () => {
    const cfg = rlConfigsFor("https://dimmy-licensing-staging.konrad-dalla.workers.dev");
    expect(cfg.trial.limit).toBe(50);
  });

  test("undefined / empty PUBLIC_URL falls back to prod (fail-closed)", () => {
    expect(rlConfigsFor(undefined).trial.limit).toBe(5);
    expect(rlConfigsFor("").trial.limit).toBe(5);
  });
});
