-- Expand the S04 durable queue for all first-party agent roles without
-- changing the retained Extractor history or attempt lineage.
CREATE TABLE durable_job_next (
  id TEXT PRIMARY KEY,
  role TEXT NOT NULL,
  status TEXT NOT NULL CHECK(status IN ('queued','running','succeeded','invalid','failed','cancelled')),
  input_json TEXT NOT NULL,
  input_sha256 TEXT NOT NULL,
  source_occurrence_id TEXT REFERENCES source_occurrence(id) ON DELETE CASCADE,
  source_variant_id TEXT REFERENCES source_variant(id) ON DELETE CASCADE,
  scope_kind TEXT NOT NULL CHECK(scope_kind IN ('source','owner','party','segment','opportunity','material')),
  scope_id TEXT NOT NULL,
  scope_revision INTEGER NOT NULL DEFAULT 1 CHECK(scope_revision > 0),
  current_attempt INTEGER NOT NULL DEFAULT 0 CHECK(current_attempt BETWEEN 0 AND 3),
  max_attempts INTEGER NOT NULL DEFAULT 3 CHECK(max_attempts BETWEEN 1 AND 3),
  progress_current INTEGER NOT NULL DEFAULT 0 CHECK(progress_current >= 0),
  progress_total INTEGER NOT NULL DEFAULT 0 CHECK(progress_total >= 0),
  last_error_code TEXT,
  last_error TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  completed_at TEXT,
  CHECK((role='extractor' AND scope_kind='source' AND source_occurrence_id IS NOT NULL AND source_variant_id IS NOT NULL)
     OR (role<>'extractor' AND source_occurrence_id IS NULL AND source_variant_id IS NULL AND scope_kind<>'source'))
);
INSERT INTO durable_job_next(
  id,role,status,input_json,input_sha256,source_occurrence_id,source_variant_id,
  scope_kind,scope_id,scope_revision,current_attempt,max_attempts,progress_current,
  progress_total,last_error_code,last_error,created_at,updated_at,completed_at
)
SELECT id,role,status,input_json,input_sha256,source_occurrence_id,source_variant_id,
       'source',source_occurrence_id,1,current_attempt,max_attempts,progress_current,
       progress_total,last_error_code,last_error,created_at,updated_at,completed_at
FROM durable_job;

CREATE TABLE job_attempt_next (
  job_id TEXT NOT NULL REFERENCES durable_job_next(id) ON DELETE CASCADE,
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
INSERT INTO job_attempt_next SELECT * FROM job_attempt;

DROP TRIGGER job_attempt_result_immutable;
DROP TRIGGER durable_job_terminal_guard;
DROP INDEX durable_job_queue;
DROP INDEX durable_job_source;
DROP TABLE job_attempt;
DROP TABLE durable_job;
ALTER TABLE durable_job_next RENAME TO durable_job;
ALTER TABLE job_attempt_next RENAME TO job_attempt;
CREATE INDEX durable_job_queue ON durable_job(status,created_at,id);
CREATE INDEX durable_job_source ON durable_job(source_occurrence_id,created_at) WHERE source_occurrence_id IS NOT NULL;
CREATE INDEX durable_job_scope ON durable_job(scope_kind,scope_id,created_at,id);
CREATE TRIGGER durable_job_terminal_guard
BEFORE UPDATE OF status ON durable_job
WHEN OLD.status IN ('succeeded','invalid','failed','cancelled') AND NEW.status<>OLD.status
BEGIN SELECT RAISE(ABORT,'terminal jobs cannot be reopened'); END;
CREATE TRIGGER job_attempt_result_immutable
BEFORE UPDATE ON job_attempt
WHEN OLD.status<>'running'
BEGIN SELECT RAISE(ABORT,'terminal job attempts are immutable'); END;

CREATE TABLE agent_chat (
  id TEXT PRIMARY KEY,
  role TEXT NOT NULL CHECK(role IN ('interviewer','briefer')),
  scope_kind TEXT NOT NULL CHECK(scope_kind IN ('owner','party','segment')),
  scope_id TEXT,
  asset_kind TEXT CHECK(asset_kind IN ('gift','proof','artifact')),
  revision INTEGER NOT NULL DEFAULT 1 CHECK(revision > 0),
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  CHECK((scope_kind='owner' AND scope_id IS NULL AND role='interviewer')
     OR (scope_kind='party' AND scope_id IS NOT NULL)
     OR (scope_kind='segment' AND scope_id IS NOT NULL AND role='briefer')),
  CHECK((role='interviewer' AND asset_kind IS NOT NULL) OR (role='briefer' AND asset_kind IS NULL))
);
CREATE INDEX agent_chat_scope ON agent_chat(role,scope_kind,scope_id,updated_at,id);

CREATE TABLE agent_turn (
  id TEXT PRIMARY KEY,
  chat_id TEXT NOT NULL REFERENCES agent_chat(id) ON DELETE CASCADE,
  turn_no INTEGER NOT NULL CHECK(turn_no > 0),
  speaker TEXT NOT NULL CHECK(speaker IN ('owner','agent')),
  text TEXT NOT NULL CHECK(length(trim(text)) > 0),
  job_id TEXT REFERENCES durable_job(id) ON DELETE SET NULL,
  created_at TEXT NOT NULL,
  UNIQUE(chat_id,turn_no)
);
CREATE INDEX agent_turn_chat ON agent_turn(chat_id,turn_no);

CREATE TABLE interviewer_question_bank_version (
  version_id TEXT PRIMARY KEY,
  version_no INTEGER NOT NULL UNIQUE CHECK(version_no > 0),
  content_json TEXT NOT NULL,
  content_sha256 TEXT NOT NULL,
  created_by TEXT NOT NULL CHECK(created_by IN ('bundled','owner')),
  created_at TEXT NOT NULL
);
CREATE TABLE interviewer_question_bank_active (
  singleton INTEGER PRIMARY KEY CHECK(singleton=1),
  version_id TEXT NOT NULL REFERENCES interviewer_question_bank_version(version_id)
);

CREATE TABLE agent_prompt_version (
  role TEXT NOT NULL CHECK(role IN ('interviewer','briefer')),
  version_no INTEGER NOT NULL CHECK(version_no > 0),
  version_id TEXT NOT NULL UNIQUE,
  prompt_text TEXT NOT NULL,
  prompt_sha256 TEXT NOT NULL,
  created_by TEXT NOT NULL CHECK(created_by IN ('bundled','owner')),
  created_at TEXT NOT NULL,
  PRIMARY KEY(role,version_no)
);
CREATE TABLE agent_prompt_active (
  role TEXT PRIMARY KEY,
  version_id TEXT NOT NULL UNIQUE REFERENCES agent_prompt_version(version_id)
);

CREATE TABLE interviewer_question_state (
  chat_id TEXT NOT NULL REFERENCES agent_chat(id) ON DELETE CASCADE,
  question_id TEXT NOT NULL,
  state TEXT NOT NULL CHECK(state IN ('asked','skipped','deferred','unknown','answered')),
  updated_at TEXT NOT NULL,
  PRIMARY KEY(chat_id,question_id)
);
CREATE TABLE interviewer_candidate (
  id TEXT PRIMARY KEY,
  chat_id TEXT NOT NULL REFERENCES agent_chat(id) ON DELETE CASCADE,
  job_id TEXT NOT NULL UNIQUE REFERENCES durable_job(id) ON DELETE CASCADE,
  asset_kind TEXT NOT NULL CHECK(asset_kind IN ('gift','proof','artifact')),
  fields_json TEXT NOT NULL,
  owner_quotes_json TEXT NOT NULL,
  missing_fields_json TEXT NOT NULL,
  state TEXT NOT NULL CHECK(state IN ('proposed','saved','dismissed')),
  resource_id TEXT REFERENCES resource(id) ON DELETE RESTRICT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  CHECK((state='saved' AND resource_id IS NOT NULL) OR (state<>'saved' AND resource_id IS NULL))
);
CREATE INDEX interviewer_candidate_chat ON interviewer_candidate(chat_id,created_at,id);

CREATE TABLE segment (
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL CHECK(length(trim(name)) BETWEEN 1 AND 200),
  description TEXT,
  revision INTEGER NOT NULL DEFAULT 1 CHECK(revision > 0),
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);
CREATE UNIQUE INDEX segment_name ON segment(name COLLATE NOCASE);
CREATE TABLE segment_party (
  segment_id TEXT NOT NULL REFERENCES segment(id) ON DELETE CASCADE,
  party_id TEXT NOT NULL REFERENCES entity(id) ON DELETE RESTRICT,
  created_at TEXT NOT NULL,
  PRIMARY KEY(segment_id,party_id)
);
CREATE INDEX segment_party_by_party ON segment_party(party_id,segment_id);

CREATE TABLE fluency (
  term TEXT PRIMARY KEY COLLATE NOCASE CHECK(length(trim(term)) BETWEEN 1 AND 120),
  status TEXT NOT NULL CHECK(status IN ('unknown','learning','fluent')),
  revision INTEGER NOT NULL DEFAULT 1 CHECK(revision > 0),
  updated_at TEXT NOT NULL
);
CREATE INDEX fluency_status ON fluency(status,term COLLATE NOCASE);

CREATE TABLE brief_item (
  id TEXT PRIMARY KEY,
  scope_kind TEXT NOT NULL CHECK(scope_kind IN ('party','segment')),
  scope_id TEXT NOT NULL,
  kind TEXT NOT NULL CHECK(kind IN ('jargon','norm','value','landmine','stakeholder','interest_hypothesis')),
  text TEXT NOT NULL CHECK(length(trim(text)) > 0),
  detail_json TEXT NOT NULL,
  tier INTEGER NOT NULL CHECK(tier IN (1,2,3)),
  source_occurrence_id TEXT REFERENCES source_occurrence(id) ON DELETE SET NULL,
  source_variant_id TEXT REFERENCES source_variant(id) ON DELETE SET NULL,
  quote TEXT,
  source_start INTEGER,
  source_end INTEGER,
  check_suggestion TEXT,
  speaker_kind TEXT NOT NULL CHECK(speaker_kind IN ('owner','party','unknown','model_prior')),
  confirmed_by_owner INTEGER NOT NULL DEFAULT 0 CHECK(confirmed_by_owner IN (0,1)),
  status TEXT NOT NULL CHECK(status IN ('proposed','confirmed','corrected','dismissed')),
  revision INTEGER NOT NULL DEFAULT 1 CHECK(revision > 0),
  supersedes_id TEXT REFERENCES brief_item(id) ON DELETE RESTRICT,
  job_id TEXT REFERENCES durable_job(id) ON DELETE SET NULL,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  CHECK((tier=1 AND quote IS NOT NULL AND source_start IS NOT NULL AND source_end>source_start
         AND source_occurrence_id IS NOT NULL AND source_variant_id IS NOT NULL)
     OR (tier=2 AND check_suggestion IS NOT NULL AND speaker_kind='model_prior')
     OR (tier=3 AND confirmed_by_owner=1 AND speaker_kind='owner'))
);
CREATE INDEX brief_scope ON brief_item(scope_kind,scope_id,status,kind,id);
CREATE INDEX brief_source ON brief_item(source_occurrence_id,source_start,source_end) WHERE source_occurrence_id IS NOT NULL;
CREATE TRIGGER brief_scope_insert_guard BEFORE INSERT ON brief_item
WHEN (NEW.scope_kind='party' AND NOT EXISTS(SELECT 1 FROM entity WHERE id=NEW.scope_id AND kind IN ('person','org')))
  OR (NEW.scope_kind='segment' AND NOT EXISTS(SELECT 1 FROM segment WHERE id=NEW.scope_id))
BEGIN SELECT RAISE(ABORT,'brief scope does not exist or has the wrong type'); END;
CREATE TRIGGER brief_scope_update_guard BEFORE UPDATE OF scope_kind,scope_id ON brief_item
WHEN (NEW.scope_kind='party' AND NOT EXISTS(SELECT 1 FROM entity WHERE id=NEW.scope_id AND kind IN ('person','org')))
  OR (NEW.scope_kind='segment' AND NOT EXISTS(SELECT 1 FROM segment WHERE id=NEW.scope_id))
BEGIN SELECT RAISE(ABORT,'brief scope does not exist or has the wrong type'); END;
