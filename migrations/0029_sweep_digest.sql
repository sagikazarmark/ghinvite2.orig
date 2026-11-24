-- Daily record of the hourly sweep watchdog (#86). Each complete check folds
-- into its UTC day: the oldest overdue chain, the latest failed sweep and
-- counts. Observations only; schedules stay authoritative in Restate.
CREATE TABLE sweep_digest_days (
    day TEXT PRIMARY KEY NOT NULL CHECK(length(day)=10),
    first_observed_at TEXT NOT NULL,
    last_observed_at TEXT NOT NULL,
    checks INTEGER NOT NULL CHECK(checks BETWEEN 1 AND 9007199254740991),
    schedules INTEGER NOT NULL CHECK(schedules BETWEEN 0 AND 9007199254740991),
    rearmed INTEGER NOT NULL CHECK(rearmed BETWEEN 0 AND 9007199254740991),
    unavailable INTEGER NOT NULL CHECK(unavailable BETWEEN 0 AND 9007199254740991),
    oldest_overdue_schedule TEXT,
    oldest_overdue_due_at TEXT,
    failed_schedules INTEGER NOT NULL CHECK(failed_schedules BETWEEN 0 AND 9007199254740991),
    latest_failed_schedule TEXT,
    latest_failed_at TEXT,
    CHECK((oldest_overdue_schedule IS NULL) = (oldest_overdue_due_at IS NULL)),
    CHECK((latest_failed_schedule IS NULL) = (latest_failed_at IS NULL))
);
