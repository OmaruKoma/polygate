-- Persistent check-in runs and per-account schedules (M12).
--
-- Why this is in SQLite instead of memory: the previous reference
-- implementation kept a per-process scope set, so a restart forgot the day's
-- check-in and a second process duplicated it. A local row cannot make the
-- *remote* side effect exactly-once; it only guarantees one local attempt per
-- (account, business date). Results that may have reached the upstream but were
-- never confirmed are stored as `unknown` and are never re-executed blindly.
--
-- `business_date` is the date in `timezone` (an IANA name), not the host's
-- local date. The unique key is the whole dedupe mechanism; the claim transition
-- (`pending|failed -> running`) is a conditional UPDATE so two workers cannot
-- both own the run.

CREATE TABLE checkin_runs (
    id                  TEXT PRIMARY KEY,
    account_id          TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    provider_kind       TEXT NOT NULL,
    region              TEXT NOT NULL,
    business_date       TEXT NOT NULL,              -- YYYY-MM-DD in `timezone`
    timezone            TEXT NOT NULL,              -- IANA name used for the date
    status              TEXT NOT NULL,
                        -- pending | running | succeeded | already_checked_in
                        -- | failed | unknown | unsupported
    attempt_count       INTEGER NOT NULL DEFAULT 0 CHECK (attempt_count >= 0),
    started_at          TEXT,
    completed_at        TEXT,
    last_error_category TEXT,
    result_summary      TEXT NOT NULL DEFAULT '',
    lease_until         TEXT,                       -- running lease; stale => interrupted
    created_at          TEXT NOT NULL,
    updated_at          TEXT NOT NULL,
    UNIQUE (account_id, business_date)
);
CREATE INDEX idx_checkin_runs_status ON checkin_runs(status);
CREATE INDEX idx_checkin_runs_date ON checkin_runs(business_date);

-- One schedule per account. A global schedule would have to encode per-provider
-- time zones anyway, and accounts are the unit an operator enables/disables.
CREATE TABLE checkin_schedules (
    account_id      TEXT PRIMARY KEY REFERENCES accounts(id) ON DELETE CASCADE,
    enabled         INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
    timezone        TEXT NOT NULL,                  -- IANA name
    local_time      TEXT NOT NULL,                  -- HH:MM, 24h, in `timezone`
    jitter_secs     INTEGER NOT NULL DEFAULT 0 CHECK (jitter_secs >= 0),
    catch_up_secs   INTEGER NOT NULL DEFAULT 3600 CHECK (catch_up_secs >= 0),
    max_attempts    INTEGER NOT NULL DEFAULT 2 CHECK (max_attempts >= 1),
    updated_at      TEXT NOT NULL
);

INSERT INTO schema_version(version, applied_at)
VALUES (3, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'));
