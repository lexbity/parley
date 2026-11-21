CREATE TABLE agent_provider (
  singleton INTEGER PRIMARY KEY CHECK(singleton=1),
  endpoint TEXT NOT NULL,
  model TEXT NOT NULL,
  model_digest TEXT,
  num_ctx INTEGER NOT NULL CHECK(num_ctx BETWEEN 1024 AND 131072),
  num_predict INTEGER NOT NULL CHECK(num_predict BETWEEN 64 AND 8192),
  locale TEXT NOT NULL,
  updated_at TEXT NOT NULL
);
INSERT INTO agent_provider(singleton,endpoint,model,num_ctx,num_predict,locale,updated_at)
VALUES(1,'http://127.0.0.1:11434','',8192,1024,'en','1970-01-01T00:00:00Z');

CREATE TABLE durable_job (
  id TEXT PRIMARY KEY,
  role TEXT NOT NULL CHECK(role IN ('extractor')),
  status TEXT NOT NULL CHECK(status IN ('queued','running','succeeded','invalid','failed','cancelled')),
  input_json TEXT NOT NULL,
  input_sha256 TEXT NOT NULL,
  source_occurrence_id TEXT NOT NULL REFERENCES source_occurrence(id) ON DELETE CASCADE,
  source_variant_id TEXT NOT NULL REFERENCES source_variant(id) ON DELETE CASCADE,
  current_attempt INTEGER NOT NULL DEFAULT 0 CHECK(current_attempt BETWEEN 0 AND 3),
  max_attempts INTEGER NOT NULL DEFAULT 3 CHECK(max_attempts BETWEEN 1 AND 3),
  progress_current INTEGER NOT NULL DEFAULT 0 CHECK(progress_current >= 0),
  progress_total INTEGER NOT NULL DEFAULT 0 CHECK(progress_total >= 0),
  last_error_code TEXT,
  last_error TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  completed_at TEXT
);
CREATE INDEX durable_job_queue ON durable_job(status,created_at,id);
CREATE INDEX durable_job_source ON durable_job(source_occurrence_id,created_at);

CREATE TABLE job_attempt (
  job_id TEXT NOT NULL REFERENCES durable_job(id) ON DELETE CASCADE,
  attempt_no INTEGER NOT NULL CHECK(attempt_no BETWEEN 1 AND 3),
  status TEXT NOT NULL CHECK(status IN ('running','succeeded','invalid','failed','cancelled','stale')),
  model TEXT NOT NULL,
  model_digest TEXT,
  provider_snapshot_json TEXT NOT NULL,
  prompt_id TEXT NOT NULL,
  prompt_sha256 TEXT NOT NULL,
  input_sha256 TEXT NOT NULL,
  input_json TEXT NOT NULL,
  output_json TEXT,
  validation_errors_json TEXT NOT NULL DEFAULT '[]',
  tokens_in INTEGER,
  tokens_out INTEGER,
  started_at TEXT NOT NULL,
  finished_at TEXT,
  duration_ms INTEGER,
  error_code TEXT,
  error_message TEXT,
  PRIMARY KEY(job_id,attempt_no)
);


-- Keep the existing proposal table and its self-referencing foreign key intact.
-- `unresolved` is a review state distinct from accepting or resolving identity.
ALTER TABLE proposal_item ADD COLUMN review_state TEXT CHECK(review_state IN ('unresolved'));

CREATE TABLE proposal_decision (
  id TEXT PRIMARY KEY,
  proposal_item_id TEXT NOT NULL UNIQUE REFERENCES proposal_item(id) ON DELETE CASCADE,
  decision TEXT NOT NULL CHECK(decision IN ('confirmed','corrected','dismissed','source_only','unresolved')),
  target_entity_id TEXT REFERENCES entity(id) ON DELETE RESTRICT,
  approved_wording TEXT,
  opportunity_id TEXT REFERENCES opportunity(id) ON DELETE RESTRICT,
  decided_at TEXT NOT NULL,
  CHECK((decision IN ('confirmed','corrected') AND target_entity_id IS NOT NULL AND approved_wording IS NOT NULL)
     OR decision IN ('dismissed','source_only','unresolved'))
);

-- A mention remains a mention until the owner resolves it. Persisting it here
-- deliberately does not create an entity or a graph edge.
CREATE TABLE unresolved_mention (
  id TEXT PRIMARY KEY,
  occurrence_id TEXT NOT NULL REFERENCES source_occurrence(id) ON DELETE RESTRICT,
  proposal_item_id TEXT NOT NULL UNIQUE REFERENCES proposal_item(id) ON DELETE RESTRICT,
  mention_name TEXT NOT NULL,
  mention_kind TEXT CHECK(mention_kind IN ('person','org')),
  source_start INTEGER NOT NULL CHECK(source_start >= 0),
  source_end INTEGER NOT NULL CHECK(source_end > source_start),
  status TEXT NOT NULL CHECK(status IN ('unresolved','linked','ignored')),
  resolved_entity_id TEXT REFERENCES entity(id) ON DELETE RESTRICT,
  created_at TEXT NOT NULL,
  CHECK((status='linked' AND resolved_entity_id IS NOT NULL)
     OR (status<>'linked' AND resolved_entity_id IS NULL))
);
CREATE INDEX unresolved_mention_queue ON unresolved_mention(status,created_at,id);


CREATE TRIGGER durable_job_terminal_guard
BEFORE UPDATE OF status ON durable_job
WHEN OLD.status IN ('succeeded','invalid','failed','cancelled') AND NEW.status<>OLD.status
BEGIN SELECT RAISE(ABORT,'terminal jobs cannot be reopened'); END;
CREATE TRIGGER job_attempt_result_immutable
BEFORE UPDATE ON job_attempt
WHEN OLD.status<>'running'
BEGIN SELECT RAISE(ABORT,'terminal job attempts are immutable'); END;
