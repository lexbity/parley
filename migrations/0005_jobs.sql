CREATE TABLE agent_job (
  id TEXT PRIMARY KEY,
  job TEXT NOT NULL CHECK(job IN ('extractor')),
  model TEXT NOT NULL,
  model_digest TEXT,
  prompt_id TEXT NOT NULL,
  prompt_version TEXT NOT NULL,
  prompt_sha256 TEXT NOT NULL CHECK(length(prompt_sha256)=64),
  params_json TEXT NOT NULL CHECK(json_valid(params_json)),
  input_sha256 TEXT NOT NULL CHECK(length(input_sha256)=64),
  input_json TEXT NOT NULL CHECK(json_valid(input_json)),
  output_json TEXT CHECK(output_json IS NULL OR json_valid(output_json)),
  status TEXT NOT NULL CHECK(status IN ('queued','running','ok','invalid','failed','cancelled')),
  validation_errors_json TEXT NOT NULL DEFAULT '[]' CHECK(json_valid(validation_errors_json)),
  dropped_items INTEGER NOT NULL DEFAULT 0 CHECK(dropped_items >= 0),
  retries INTEGER NOT NULL DEFAULT 0 CHECK(retries BETWEEN 0 AND 2),
  tokens_in INTEGER,
  tokens_out INTEGER,
  progress_json TEXT NOT NULL DEFAULT '{}' CHECK(json_valid(progress_json)),
  started_at TEXT,
  completed_at TEXT,
  duration_ms INTEGER,
  created_at TEXT NOT NULL
);
CREATE INDEX agent_job_created ON agent_job(created_at DESC,id);

ALTER TABLE proposal_batch ADD COLUMN revision INTEGER NOT NULL DEFAULT 1 CHECK(revision > 0);
ALTER TABLE proposal_batch ADD COLUMN status_detail TEXT;

CREATE TABLE capture_review_item (
  id TEXT PRIMARY KEY,
  batch_id TEXT NOT NULL REFERENCES proposal_batch(id) ON DELETE RESTRICT,
  action TEXT NOT NULL CHECK(action IN ('create_claim','create_entity','create_edge','link_source')),
  target_kind TEXT NOT NULL CHECK(target_kind IN ('entity','claim','edge','opportunity','none')),
  target_id TEXT,
  target_revision INTEGER,
  label TEXT NOT NULL,
  quote TEXT NOT NULL,
  tier INTEGER NOT NULL CHECK(tier IN (1,2,3)),
  ambiguity TEXT,
  unresolved INTEGER NOT NULL DEFAULT 0 CHECK(unresolved IN (0,1)),
  candidate_json TEXT NOT NULL CHECK(json_valid(candidate_json)),
  quote_start INTEGER NOT NULL CHECK(quote_start >= 0),
  quote_end INTEGER NOT NULL CHECK(quote_end > quote_start),
  normalized_start INTEGER NOT NULL CHECK(normalized_start >= 0),
  normalized_end INTEGER NOT NULL CHECK(normalized_end > normalized_start),
  normalization_version INTEGER NOT NULL,
  status TEXT NOT NULL DEFAULT 'pending' CHECK(status IN ('pending','confirmed','corrected','dismissed','unresolved','asked_in_chat')),
  created_at TEXT NOT NULL,
  CHECK(unresolved=0 OR target_id IS NULL)
);
CREATE INDEX capture_review_item_batch ON capture_review_item(batch_id,status,id);

CREATE TABLE capture_review_decision (
  id TEXT PRIMARY KEY,
  batch_id TEXT NOT NULL REFERENCES proposal_batch(id) ON DELETE RESTRICT,
  proposal_id TEXT NOT NULL REFERENCES capture_review_item(id) ON DELETE RESTRICT,
  action TEXT NOT NULL CHECK(action IN ('confirm','correct','dismiss','unresolved','ask_in_chat')),
  target_id TEXT,
  corrected_fields_json TEXT CHECK(corrected_fields_json IS NULL OR json_valid(corrected_fields_json)),
  resulting_entity_id TEXT REFERENCES entity(id) ON DELETE RESTRICT,
  resulting_claim_id TEXT REFERENCES claim(id) ON DELETE RESTRICT,
  revision INTEGER NOT NULL,
  created_at TEXT NOT NULL,
  UNIQUE(batch_id,proposal_id)
);
CREATE TRIGGER capture_decision_no_update BEFORE UPDATE ON capture_review_decision
BEGIN SELECT RAISE(ABORT,'capture decisions are append-only'); END;
CREATE TRIGGER capture_decision_no_delete BEFORE DELETE ON capture_review_decision
BEGIN SELECT RAISE(ABORT,'capture decisions are append-only'); END;

CREATE TABLE capture_batch_opportunity (
  batch_id TEXT NOT NULL REFERENCES proposal_batch(id) ON DELETE RESTRICT,
  opportunity_id TEXT NOT NULL REFERENCES opportunity(id) ON DELETE RESTRICT,
  created_at TEXT NOT NULL,
  PRIMARY KEY(batch_id,opportunity_id)
);

CREATE TRIGGER agent_job_no_delete BEFORE DELETE ON agent_job
BEGIN SELECT RAISE(ABORT,'job history is retained'); END;
