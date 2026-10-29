-- Persisted model aliases. One alias may have several targets (multi-target
-- routing). Native model ids stay resolvable; an alias must not shadow one.
CREATE TABLE model_routes (
    id             TEXT PRIMARY KEY,
    alias          TEXT NOT NULL,
    provider       TEXT NOT NULL,
    provider_kind  TEXT NOT NULL,
    region         TEXT NOT NULL,
    upstream_model TEXT NOT NULL,
    display_name   TEXT,
    enabled        INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
    priority       INTEGER NOT NULL DEFAULT 0,
    weight         INTEGER NOT NULL DEFAULT 1 CHECK (weight >= 1),
    capabilities   TEXT NOT NULL DEFAULT '{}',
    created_at     TEXT NOT NULL,
    updated_at     TEXT NOT NULL
);

CREATE UNIQUE INDEX idx_model_routes_target
    ON model_routes(alias, provider, upstream_model);
CREATE INDEX idx_model_routes_alias ON model_routes(alias, enabled);

INSERT INTO schema_version(version, applied_at)
VALUES (7, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'));
