-- Short-lived cross-process lease around a credential refresh.
--
-- Refresh tokens rotate: two processes exchanging the same refresh token can
-- invalidate each other. A process-wide mutex is not enough when one database is
-- shared by several gateway processes, so the lease lives in SQLite. It is
-- advisory and time-bounded; the credential revision CAS still decides who may
-- write the new value.
CREATE TABLE credential_refresh_leases (
    account_id TEXT PRIMARY KEY REFERENCES accounts(id) ON DELETE CASCADE,
    owner      TEXT NOT NULL,
    -- Unix seconds; an expired lease may be taken by anyone.
    expires_at INTEGER NOT NULL
);

INSERT INTO schema_version(version, applied_at)
VALUES (6, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'));
