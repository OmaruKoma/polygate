-- Optimistic concurrency for credential updates.
--
-- M11 needs a compare-and-swap token so two admin sessions cannot silently
-- overwrite each other's secret. `updated_at` is not sufficient: two updates in
-- the same millisecond would collide. `revision` starts at 0 for existing rows
-- and increments on every successful write.

ALTER TABLE credentials ADD COLUMN revision INTEGER NOT NULL DEFAULT 0;

INSERT INTO schema_version(version, applied_at)
VALUES (2, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'));
