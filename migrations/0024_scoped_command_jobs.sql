-- UCR-001 U05: scoped command jobs (brief.start / job.get_status).
--
-- A job started by a registered command carries the initiating run, its
-- epoch and the owner pseudonym so admission, completion and replay can be
-- fenced against revocation, epoch rotation and target scope. Existing
-- owner-launched jobs keep a zero epoch and a null run binding and remain
-- governed by their own scope/revision checks.

ALTER TABLE durable_job ADD COLUMN epoch TEXT NOT NULL DEFAULT '';
ALTER TABLE durable_job ADD COLUMN run_id TEXT;
ALTER TABLE durable_job ADD COLUMN owner_pseudonym TEXT;

CREATE INDEX durable_job_scoped_run
  ON durable_job(run_id, epoch)
  WHERE run_id IS NOT NULL;
