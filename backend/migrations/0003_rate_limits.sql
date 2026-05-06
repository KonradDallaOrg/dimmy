-- 0003_rate_limits.sql — token-bucket rate limiter state.
--
-- One row per (namespace, identity) pair. window_start is the epoch
-- second when the current rate window opened. count increments on
-- each call within the window. When `now - window_start >= period`
-- the row resets (count := 1, window_start := now) atomically via
-- the UPSERT used in src/rate-limit.ts.
--
-- Pruning: stale rows live until they're touched by a new request;
-- D1 storage is cheap enough that a periodic GC isn't worth the code.
CREATE TABLE IF NOT EXISTS rate_limits (
  bucket       TEXT    PRIMARY KEY,  -- "namespace:identity" e.g. "trial:1.2.3.4"
  window_start INTEGER NOT NULL,
  count        INTEGER NOT NULL
);
