-- Session binding revision and explicit invalidation (M13).
--
-- Why a revision: the binding is updated by concurrent requests (upstream chat id
-- after the first turn, operator rebind, invalidation). A plain last-writer-wins
-- UPDATE can silently move a live session to another account or overwrite the
-- upstream id captured by the account it no longer belongs to. Every binding
-- change therefore goes through a compare-and-swap on `binding_revision`.
--
-- Why invalid_reason: an upstream session that is no longer usable must be
-- recorded, not deleted. The row keeps its account and (until an explicit reset)
-- its upstream id so the operator can see what was lost; acquisition then fails
-- with `session_binding_invalid` until a recovery action is taken.

ALTER TABLE sessions ADD COLUMN binding_revision INTEGER NOT NULL DEFAULT 0;
ALTER TABLE sessions ADD COLUMN invalid_reason TEXT;

INSERT INTO schema_version(version, applied_at)
VALUES (4, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'));
