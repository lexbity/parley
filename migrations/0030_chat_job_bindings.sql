-- AR04: typed Interviewer/Briefer chat-job bindings.
--
-- Role jobs started by an owner turn now bind the exact agent_chat they
-- belong to through a dedicated column, so chat summaries and active-job
-- lookups never scan raw job JSON. Unfinished pre-cut Interviewer/Briefer
-- rows are terminalized before the replacement intake accepts new work; the
-- new intake freezes the reviewed prompt version and the qualified profile at
-- admission.
PRAGMA defer_foreign_keys = ON;

ALTER TABLE durable_job ADD COLUMN chat_id TEXT REFERENCES agent_chat(id) ON DELETE SET NULL;

-- Backfill validated bindings from rows that were already terminal when this
-- migration runs. Only rows whose extracted chat reference resolves to a real
-- chat of the same role are bound; anything else stays unbound history.
UPDATE durable_job
SET chat_id = scope_id
WHERE role = 'interviewer' AND scope_kind = 'owner'
  AND chat_id IS NULL AND scope_id <> '' AND status IN ('succeeded','invalid','failed','cancelled')
  AND EXISTS (
    SELECT 1 FROM agent_chat c
    WHERE c.id = durable_job.scope_id AND c.role = 'interviewer' AND c.scope_kind = 'owner'
  );

UPDATE durable_job
SET chat_id = CASE
  WHEN json_type(json_extract(input_json, '$.chat_id')) = 'text'
    THEN json_extract(input_json, '$.chat_id')
  ELSE NULL
END
WHERE role = 'briefer' AND chat_id IS NULL AND status IN ('succeeded','invalid','failed','cancelled')
  AND json_valid(input_json)
  AND EXISTS (
    SELECT 1 FROM agent_chat c
    WHERE c.id = json_extract(input_json, '$.chat_id') AND c.role = 'briefer'
  );

-- Interrupt unfinished pre-cut role work with a stable lineage outcome. The
-- owner can retry from the current scope; no old job can open the new path.
CREATE TEMP TABLE ar04_interrupt AS
SELECT id FROM durable_job
WHERE role IN ('interviewer','briefer') AND status IN ('queued','running');

UPDATE job_attempt
SET status = 'failed', error_code = 'INTERRUPTED_UPGRADE',
    finished_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
WHERE status = 'running' AND job_id IN (SELECT id FROM ar04_interrupt);

UPDATE durable_job
SET status = 'failed', last_error_code = 'INTERRUPTED_UPGRADE', last_error = NULL,
    completed_at = strftime('%Y-%m-%dT%H:%M:%fZ','now'),
    updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
WHERE id IN (SELECT id FROM ar04_interrupt);

DROP TABLE ar04_interrupt;

-- Active work lookup by chat: one indexed path for both roles, replacing the
-- former JSON scan in chat summaries and detail reads.
CREATE INDEX durable_job_chat_active ON durable_job(chat_id, status) WHERE chat_id IS NOT NULL;
