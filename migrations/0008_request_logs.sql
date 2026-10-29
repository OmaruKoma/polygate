-- Non-sensitive per-request log. Prompts, messages, tool arguments and
-- credentials are never stored here.
CREATE TABLE request_logs (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    request_id    TEXT NOT NULL,
    provider      TEXT,
    provider_kind TEXT NOT NULL,
    region        TEXT,
    account_id    TEXT,
    model_alias   TEXT,
    upstream_model TEXT,
    api           TEXT NOT NULL,
    stream        INTEGER NOT NULL DEFAULT 0 CHECK (stream IN (0, 1)),
    http_status   INTEGER NOT NULL,
    error_category TEXT,
    input_tokens  INTEGER NOT NULL DEFAULT 0,
    output_tokens INTEGER NOT NULL DEFAULT 0,
    latency_ms    INTEGER NOT NULL DEFAULT 0,
    created_at    TEXT NOT NULL
);

CREATE INDEX idx_request_logs_created ON request_logs(created_at);
CREATE INDEX idx_request_logs_account ON request_logs(account_id, created_at);

INSERT INTO schema_version(version, applied_at)
VALUES (8, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'));
