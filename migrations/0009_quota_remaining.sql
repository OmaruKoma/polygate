-- Some upstreams report a spendable balance without a granted total
-- (TeleAgent: `current_points_balance` only). `remaining` used to be derived as
-- `limit_value - used`, which silently dropped such balances on reload.
-- Store it explicitly; legacy rows fall back to the derived value.

ALTER TABLE quota_snapshots ADD COLUMN remaining_value REAL;

UPDATE quota_snapshots
SET remaining_value = limit_value - used
WHERE remaining_value IS NULL AND limit_value IS NOT NULL AND used IS NOT NULL;

INSERT INTO schema_version(version, applied_at)
VALUES (9, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'));
