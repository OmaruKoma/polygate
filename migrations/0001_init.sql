-- PolyGate schema, version 1.
--
-- Connection pragmas (WAL, busy_timeout, foreign_keys) are applied per pooled
-- connection in src/storage/mod.rs. `PRAGMA journal_mode` cannot change inside
-- the transaction sqlx wraps around a migration, so it is not set here.

CREATE TABLE schema_version (
    version    INTEGER PRIMARY KEY,
    applied_at TEXT NOT NULL
);

CREATE TABLE accounts (
    id              TEXT PRIMARY KEY,
    provider        TEXT NOT NULL,
    provider_kind   TEXT NOT NULL,
    region          TEXT NOT NULL,
    display_name    TEXT NOT NULL,
    credential_hint TEXT NOT NULL DEFAULT '',
    enabled         INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
    weight          INTEGER NOT NULL DEFAULT 1 CHECK (weight >= 1),
    max_concurrency INTEGER NOT NULL DEFAULT 4 CHECK (max_concurrency >= 1),
    quota_group     TEXT,
    auth_status     TEXT NOT NULL DEFAULT 'unknown',
    auth_checked_at TEXT,
    last_error      TEXT,
    metadata        TEXT NOT NULL DEFAULT '{}',
    created_at      TEXT NOT NULL,
    updated_at      TEXT NOT NULL
);
CREATE INDEX idx_accounts_provider ON accounts(provider_kind, region);
CREATE INDEX idx_accounts_quota_group ON accounts(quota_group);

-- Secrets live here and nowhere else. `encrypted_payload` is a SecretBox
-- ciphertext bound to `account_id`; plaintext credentials are never stored.
CREATE TABLE credentials (
    account_id         TEXT PRIMARY KEY REFERENCES accounts(id) ON DELETE CASCADE,
    encrypted_payload  TEXT NOT NULL,
    encryption_version TEXT NOT NULL,
    created_at         TEXT NOT NULL,
    updated_at         TEXT NOT NULL
);

CREATE TABLE sessions (
    id                  TEXT PRIMARY KEY,
    client_session_id   TEXT NOT NULL,
    provider            TEXT NOT NULL,
    provider_kind       TEXT NOT NULL,
    region              TEXT NOT NULL,
    account_id          TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    upstream_session_id TEXT,
    state               TEXT NOT NULL DEFAULT 'active',
    created_at          TEXT NOT NULL,
    last_used_at        TEXT NOT NULL,
    expires_at          TEXT NOT NULL,
    request_count       INTEGER NOT NULL DEFAULT 0 CHECK (request_count >= 0),
    metadata            TEXT NOT NULL DEFAULT '{}',
    UNIQUE (client_session_id, provider_kind, region)
);
CREATE INDEX idx_sessions_account ON sessions(account_id);
CREATE INDEX idx_sessions_expiry ON sessions(expires_at);

-- `used`/`limit_value` are NULL when the balance is unknown; they are never
-- written as 0 to mean "unknown".
CREATE TABLE quota_snapshots (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    account_id     TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    quota_group_id TEXT,
    model          TEXT,
    quota_unit     TEXT NOT NULL DEFAULT 'unknown',
    used           REAL,
    limit_value    REAL,
    status         TEXT NOT NULL DEFAULT 'unknown',
    error          TEXT,
    refreshed_at   TEXT NOT NULL,
    raw            TEXT
);
CREATE INDEX idx_quota_account ON quota_snapshots(account_id, refreshed_at DESC);
CREATE INDEX idx_quota_group ON quota_snapshots(quota_group_id, refreshed_at DESC);

CREATE TABLE usage_events (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    request_id    TEXT NOT NULL UNIQUE,
    provider_kind TEXT NOT NULL,
    account_id    TEXT REFERENCES accounts(id) ON DELETE SET NULL,
    model         TEXT,
    input_tokens  INTEGER NOT NULL DEFAULT 0 CHECK (input_tokens >= 0),
    output_tokens INTEGER NOT NULL DEFAULT 0 CHECK (output_tokens >= 0),
    status        TEXT NOT NULL,
    created_at    TEXT NOT NULL
);
CREATE INDEX idx_usage_created ON usage_events(created_at);
CREATE INDEX idx_usage_account ON usage_events(account_id, created_at);

-- Non-sensitive operator settings only (repository rejects secret-like keys).
CREATE TABLE settings (
    key        TEXT PRIMARY KEY,
    value      TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

INSERT INTO schema_version(version, applied_at)
VALUES (1, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'));