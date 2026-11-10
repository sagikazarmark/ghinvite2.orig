-- Pause epochs identify release/abort messages without changing link guardrails.
ALTER TABLE invitation_links ADD COLUMN pause_epoch INTEGER
    CHECK(pause_epoch BETWEEN 1 AND 9007199254740991);
