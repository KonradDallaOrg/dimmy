import { describe, expect, test } from "vitest";
import { MAX_OFFLINE_DAYS, SCOPES, SCOPES_FOR_TIER } from "../src/scopes";

describe("scope vocabulary", () => {
  test("scopes cover every advertised pro capability", () => {
    expect(SCOPES.MANAGED_STT).toBe("managed_stt");
    expect(SCOPES.MANAGED_LLM).toBe("managed_llm");
    expect(SCOPES.AUTO_UPDATE).toBe("auto_update");
    expect(SCOPES.HISTORY_SYNC).toBe("history_sync");
    expect(SCOPES.PREMIUM_STYLES).toBe("premium_styles");
  });
});

describe("SCOPES_FOR_TIER", () => {
  test("all 4 tiers have non-empty scope lists", () => {
    for (const tier of ["trial", "monthly", "annual", "lifetime"] as const) {
      expect(SCOPES_FOR_TIER[tier].length).toBeGreaterThan(0);
    }
  });

  test("trial unlocks the full vetrina (every paid scope)", () => {
    const all = Object.values(SCOPES);
    for (const s of all) {
      expect(SCOPES_FOR_TIER.trial).toContain(s);
    }
  });

  test("monthly is missing history_sync — that's the differentiator", () => {
    expect(SCOPES_FOR_TIER.monthly).not.toContain(SCOPES.HISTORY_SYNC);
    expect(SCOPES_FOR_TIER.monthly).toContain(SCOPES.MANAGED_STT);
    expect(SCOPES_FOR_TIER.monthly).toContain(SCOPES.AUTO_UPDATE);
  });

  test("annual + lifetime include history_sync", () => {
    expect(SCOPES_FOR_TIER.annual).toContain(SCOPES.HISTORY_SYNC);
    expect(SCOPES_FOR_TIER.lifetime).toContain(SCOPES.HISTORY_SYNC);
  });

  test("every scope in every tier is from the SCOPES vocabulary", () => {
    const vocab = new Set<string>(Object.values(SCOPES));
    for (const tier of Object.keys(SCOPES_FOR_TIER) as Array<
      keyof typeof SCOPES_FOR_TIER
    >) {
      for (const s of SCOPES_FOR_TIER[tier]) {
        expect(vocab.has(s)).toBe(true);
      }
    }
  });
});

describe("MAX_OFFLINE_DAYS", () => {
  test("monthly is the strictest grace (catches lapsed cards quickly)", () => {
    expect(MAX_OFFLINE_DAYS.monthly).toBeLessThan(MAX_OFFLINE_DAYS.annual);
  });

  test("lifetime grace covers the entire 3-year prepay window", () => {
    expect(MAX_OFFLINE_DAYS.lifetime).toBeGreaterThanOrEqual(1095);
  });

  test("trial grace is generous (30 days)", () => {
    expect(MAX_OFFLINE_DAYS.trial).toBe(30);
  });

  test("matches the values asserted in core/src/license.rs", () => {
    // These four values are the source of truth — when they change,
    // update Tier::default_max_offline_days in Rust + this assertion.
    expect(MAX_OFFLINE_DAYS).toEqual({
      trial: 30,
      monthly: 14,
      annual: 30,
      lifetime: 1095,
    });
  });
});
