-- Existing links use the immutable default share; new projections always write it.
UPDATE invitation_links SET budget_share_pct = 50 WHERE budget_share_pct IS NULL;
