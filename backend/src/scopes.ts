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

export type Tier = "trial" | "annual" | "3year";

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
  annual: [
    SCOPES.MANAGED_STT,
    SCOPES.MANAGED_LLM,
    SCOPES.AUTO_UPDATE,
    SCOPES.PREMIUM_STYLES,
  ],
  "3year": [
    SCOPES.MANAGED_STT,
    SCOPES.MANAGED_LLM,
    SCOPES.AUTO_UPDATE,
    SCOPES.HISTORY_SYNC,
    SCOPES.PREMIUM_STYLES,
  ],
};
