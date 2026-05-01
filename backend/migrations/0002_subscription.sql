-- Subscription support — additive columns.
-- Backward-compatible: existing one-time licenses keep working with NULLs.
--
-- stripe_subscription_id: present for tier in (monthly, annual). NULL for
--   tier in (trial, lifetime) which are one-time / non-recurring.
-- current_period_end:     unix timestamp of current Stripe billing period
--   end. Bumped on each invoice.paid; used to drive valid_until.
-- cancel_at_period_end:   1 if user cancelled but still in active period;
--   when current_period_end passes we revoke without further webhook.

ALTER TABLE licenses ADD COLUMN stripe_subscription_id TEXT;
ALTER TABLE licenses ADD COLUMN current_period_end INTEGER;
ALTER TABLE licenses ADD COLUMN cancel_at_period_end INTEGER NOT NULL DEFAULT 0;

CREATE INDEX IF NOT EXISTS idx_licenses_stripe_sub
  ON licenses(stripe_subscription_id);
