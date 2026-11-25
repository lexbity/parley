-- AR-001 AR08. Remove the last direct provider configuration surface.
-- The profile store (0027) is the single authority for endpoint, model and
-- budget preferences; a legacy `agent_provider` row was carried into the
-- profile store at 0027 with `localhost` mapped to numeric loopback once.
-- This migration drops the table after verifying zero remaining references
-- and terminalizes queued/running chat-role work that never received a frozen
-- profile so it cannot be claimed by the packaged worker without a qualified
-- tuple (fail closed, AR-001 FR-12).

PRAGMA defer_foreign_keys = ON;

UPDATE job_attempt
SET status = 'failed', error_code = 'INTERRUPTED_UPGRADE',
    finished_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
WHERE status = 'running' AND job_id IN (
  SELECT id FROM durable_job
  WHERE role IN ('interviewer','briefer','composer','linter')
    AND profile_revision IS NULL AND status IN ('queued','running')
);

UPDATE durable_job
SET status = 'failed', last_error_code = 'INTERRUPTED_UPGRADE',
    completed_at = strftime('%Y-%m-%dT%H:%M:%fZ','now'),
    updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now'),
    lease_id = NULL, process_nonce = NULL
WHERE role IN ('interviewer','briefer','composer','linter')
  AND profile_revision IS NULL AND status IN ('queued','running');

DROP TABLE agent_provider;

CREATE TEMP TABLE ar08_preflight (check_id INTEGER);
CREATE TEMP TRIGGER ar08_reject_orphans BEFORE INSERT ON ar08_preflight
WHEN EXISTS (SELECT 1 FROM pragma_foreign_key_check)
BEGIN SELECT RAISE(ABORT, 'AR08_FK_ORPHAN'); END;
INSERT INTO ar08_preflight VALUES (1);
DROP TRIGGER ar08_reject_orphans;
DROP TABLE ar08_preflight;
