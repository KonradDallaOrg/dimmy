// Capability vocabulary — kept in sync with core/src/license.rs::scopes.
// Adding one means updating the SCOPES_FOR_TIER table below; tokens
// minted on the next /activate or /refresh propagate the change.

export const SCOPES = {
  MANAGED_STT: "managed_stt",
  MANAGED_LLM: "managed_llm",
  AUTO_UPDATE: "auto_update",
  HISTORY_SYNC: "history_sync",
  PREMIUM_STYLES: "premium_styles",
} as const;

export type Tier = "trial" | "monthly" | "annual" | "lifetime";

export const SCOPES_FOR_TIER: Record<Tier, string[]> = {
  // Trial = full vetrina; let prospects evaluate every paid feature
  // before deciding to buy.
  trial: [
    SCOPES.MANAGED_STT,
    SCOPES.MANAGED_LLM,
    SCOPES.AUTO_UPDATE,
    SCOPES.HISTORY_SYNC,
    SCOPES.PREMIUM_STYLES,
  ],
  // Cheapest entry tier — Pro features minus history sync (which is
  // the differentiator nudging users to a higher commitment).
  monthly: [
    SCOPES.MANAGED_STT,
    SCOPES.MANAGED_LLM,
    SCOPES.AUTO_UPDATE,
    SCOPES.PREMIUM_STYLES,
  ],
  annual: [
    SCOPES.MANAGED_STT,
    SCOPES.MANAGED_LLM,
    SCOPES.AUTO_UPDATE,
    SCOPES.HISTORY_SYNC,
    SCOPES.PREMIUM_STYLES,
  ],
  // Top tier: same as annual + 3y validity. Everything unlocked.
  lifetime: [
    SCOPES.MANAGED_STT,
    SCOPES.MANAGED_LLM,
    SCOPES.AUTO_UPDATE,
    SCOPES.HISTORY_SYNC,
    SCOPES.PREMIUM_STYLES,
  ],
};

// Tier → max-offline-days mapping. Single source of truth used by
// /api/activate + /api/refresh to set the `max_offline` claim.
//
// monthly is shorter than annual on purpose: a user whose card stops
// working should notice the app degrading before they've used a full
// month of unpaid service.
export const MAX_OFFLINE_DAYS: Record<Tier, number> = {
  trial: 30,
  monthly: 14,
  annual: 30,
  lifetime: 1095,
};
