-- Web admin panel (M13-C1).
--
-- `accounts.external_id` is the upstream identity of an imported/authorized
-- account within one `(provider_kind, region)`. The CodeArts OAuth flow uses
-- `codearts:<region>:<principal_id>` so re-authorizing updates the same row
-- instead of creating a duplicate.
--
-- Admin passwords and recovery codes are stored as Argon2id / SHA-256 hashes.
-- Login sessions store only a hash of the cookie token. Pending authorization
-- flows store PKCE verifier and DPoP private JWK encrypted with SecretBox.

ALTER TABLE accounts ADD COLUMN external_id TEXT;
CREATE UNIQUE INDEX idx_accounts_external
    ON accounts(provider_kind, region, external_id)
    WHERE external_id IS NOT NULL;

CREATE TABLE admin_users (
    singleton      INTEGER PRIMARY KEY CHECK (singleton = 1),
    username       TEXT NOT NULL DEFAULT 'admin',
    password_hash  TEXT NOT NULL,
    recovery_hash  TEXT NOT NULL,
    created_at     TEXT NOT NULL,
    updated_at     TEXT NOT NULL
);

CREATE TABLE admin_sessions (
    id           TEXT PRIMARY KEY,
    token_hash   TEXT NOT NULL UNIQUE,
    csrf_token   TEXT NOT NULL,
    created_at   TEXT NOT NULL,
    last_seen_at TEXT NOT NULL,
    expires_at   TEXT NOT NULL,
    revoked_at   TEXT
);
CREATE INDEX idx_admin_sessions_expiry ON admin_sessions(expires_at);

CREATE TABLE admin_auth_flows (
    id                TEXT PRIMARY KEY,
    kind              TEXT NOT NULL,
    region            TEXT NOT NULL,
    status            TEXT NOT NULL,
    authorize_url     TEXT NOT NULL,
    redirect_uri      TEXT NOT NULL,
    ticket_id         TEXT NOT NULL,
    secrets_encrypted TEXT NOT NULL,
    requested_name    TEXT,
    account_id        TEXT,
    error             TEXT,
    created_at        TEXT NOT NULL,
    expires_at        TEXT NOT NULL,
    completed_at      TEXT
);
CREATE INDEX idx_admin_auth_flows_status ON admin_auth_flows(status, expires_at);

-- The source callback carries only `code`; allow at most one live flow.
CREATE UNIQUE INDEX idx_admin_auth_flows_single_active
    ON admin_auth_flows(status)
    WHERE status IN ('pending', 'exchanging');

INSERT INTO schema_version(version, applied_at)
VALUES (5, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'));
