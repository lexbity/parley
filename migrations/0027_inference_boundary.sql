-- AR-001 AR01. Keep product lineage separate from private execution state.
-- The entire migration runs in SQLx's transaction with foreign keys enabled.
PRAGMA defer_foreign_keys = ON;

-- Detect an orphan even when a previous backup wrote with foreign keys off.
CREATE TEMP TABLE ar01_preflight (check_id INTEGER);
CREATE TEMP TRIGGER ar01_reject_orphans BEFORE INSERT ON ar01_preflight
WHEN EXISTS (SELECT 1 FROM pragma_foreign_key_check)
  OR EXISTS (
    SELECT 1 FROM match_run m LEFT JOIN durable_job j ON j.id = m.why_job_id
    WHERE m.why_job_id IS NOT NULL AND j.id IS NULL
  )
  OR EXISTS (
    SELECT 1 FROM proposal_batch b LEFT JOIN durable_job j ON j.id = b.job_id
    WHERE b.job_id IS NOT NULL AND (j.id IS NULL OR j.source_occurrence_id IS NULL
           OR j.source_variant_id <> b.source_variant_id)
  )
BEGIN SELECT RAISE(ABORT, 'INFERENCE_LINEAGE_ORPHAN'); END;
INSERT INTO ar01_preflight VALUES (1);
DROP TRIGGER ar01_reject_orphans;

CREATE TABLE inference_profile (
  revision INTEGER PRIMARY KEY CHECK (revision > 0),
  runner_kind TEXT NOT NULL CHECK (runner_kind = 'pi'),
  runner_build_digest TEXT,
  protocol_major INTEGER NOT NULL CHECK (protocol_major = 1),
  provider_version TEXT,
  endpoint TEXT,
  chat_model TEXT,
  chat_model_digest TEXT,
  embed_model TEXT,
  embed_model_digest TEXT,
  tokenizer_digest TEXT,
  recipe_json TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(recipe_json) AND json_type(recipe_json) = 'object'),
  context_tokens INTEGER NOT NULL CHECK (context_tokens BETWEEN 1024 AND 131072),
  output_tokens INTEGER NOT NULL CHECK (output_tokens BETWEEN 64 AND 8192),
  locale TEXT NOT NULL,
  created_at TEXT NOT NULL
);
CREATE TRIGGER inference_profile_immutable BEFORE UPDATE ON inference_profile
BEGIN SELECT RAISE(ABORT, 'inference profiles are immutable'); END;

CREATE TABLE inference_active_profile (
  singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
  profile_revision INTEGER NOT NULL REFERENCES inference_profile(revision) ON DELETE RESTRICT
);
CREATE TABLE inference_profile_revocation (
  profile_revision INTEGER PRIMARY KEY REFERENCES inference_profile(revision) ON DELETE CASCADE,
  diagnostic TEXT NOT NULL,
  revoked_at TEXT NOT NULL
);
CREATE TABLE inference_qualification (
  tuple_digest TEXT PRIMARY KEY,
  profile_revision INTEGER NOT NULL REFERENCES inference_profile(revision) ON DELETE CASCADE,
  mode TEXT NOT NULL CHECK (mode IN ('structured_role','delegated','embed')),
  evaluation_digest TEXT NOT NULL CHECK (
    length(evaluation_digest) = 64 AND evaluation_digest NOT GLOB '*[^0-9a-f]*'
  ),
  checked_at TEXT NOT NULL,
  status TEXT NOT NULL CHECK (status IN ('ready','failed')),
  diagnostic TEXT
);
CREATE INDEX inference_qualification_mode
ON inference_qualification(profile_revision,mode,checked_at DESC);

CREATE TABLE inference_probe (
  id TEXT PRIMARY KEY,
  profile_revision INTEGER NOT NULL REFERENCES inference_profile(revision) ON DELETE RESTRICT,
  epoch TEXT NOT NULL,
  status TEXT NOT NULL CHECK (status IN ('queued','running','succeeded','failed','cancelled')),
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  finished_at TEXT,
  diagnostic TEXT,
  lease_id TEXT,
  CHECK ((status = 'running') = (lease_id IS NOT NULL)),
  CHECK ((status IN ('succeeded','failed','cancelled')) = (finished_at IS NOT NULL))
);
CREATE UNIQUE INDEX inference_probe_active ON inference_probe((1))
WHERE status IN ('queued','running');
CREATE INDEX inference_probe_revision ON inference_probe(profile_revision,created_at DESC,id);

CREATE TRIGGER inference_active_profile_requires_evaluation_on_insert
BEFORE INSERT ON inference_active_profile
WHEN NOT EXISTS (
  SELECT 1 FROM inference_qualification q
  WHERE q.profile_revision = NEW.profile_revision AND q.status = 'ready'
)
OR EXISTS (
  SELECT 1 FROM inference_profile_revocation r WHERE r.profile_revision = NEW.profile_revision
)
BEGIN SELECT RAISE(ABORT, 'profile needs a qualified capability'); END;

CREATE TRIGGER inference_active_profile_requires_evaluation_on_update
BEFORE UPDATE ON inference_active_profile
WHEN NOT EXISTS (
  SELECT 1 FROM inference_qualification q
  WHERE q.profile_revision = NEW.profile_revision AND q.status = 'ready'
)
OR EXISTS (
  SELECT 1 FROM inference_profile_revocation r WHERE r.profile_revision = NEW.profile_revision
)
BEGIN SELECT RAISE(ABORT, 'profile needs a qualified capability'); END;

-- Carry legacy preferences forward without assigning a qualified model digest.
WITH normalized AS (
  SELECT CASE
           WHEN endpoint LIKE 'http://localhost:%' COLLATE BINARY
                AND substr(endpoint,1,17) = 'http://localhost:'
           THEN 'http://127.0.0.1:' || substr(endpoint,18)
           WHEN endpoint = 'http://localhost' THEN 'http://127.0.0.1:80'
           ELSE endpoint
         END AS endpoint,
         model,embed_model,num_ctx,num_predict,locale,updated_at
  FROM agent_provider
), port_parts AS (
  SELECT CASE
           WHEN substr(endpoint,1,17) = 'http://127.0.0.1:' THEN substr(endpoint,18)
           WHEN substr(endpoint,1,13) = 'http://[::1]:' THEN substr(endpoint,14)
         END AS port, normalized.*
  FROM normalized
)
INSERT INTO inference_profile(
  revision,runner_kind,protocol_major,endpoint,chat_model,embed_model,
  context_tokens,output_tokens,locale,created_at
)
SELECT 1,'pi',1,
       CASE WHEN endpoint IN ('http://127.0.0.1','http://[::1]') THEN endpoint || ':80'
            WHEN length(port) BETWEEN 1 AND 5
             AND port NOT GLOB '*[^0-9]*'
             AND cast(port AS INTEGER) BETWEEN 1 AND 65535
             AND cast(cast(port AS INTEGER) AS TEXT) = port
            THEN endpoint END,
       nullif(model,''),nullif(embed_model,''),num_ctx,num_predict,locale,updated_at
FROM port_parts;
INSERT INTO inference_profile_revocation(profile_revision,diagnostic,revoked_at)
SELECT revision,'MODEL_UNAVAILABLE',created_at FROM inference_profile WHERE endpoint IS NULL;

UPDATE job_attempt
SET status = 'failed', error_code = 'INTERRUPTED_UPGRADE',
    finished_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
WHERE status = 'running' AND job_id IN (
  SELECT id FROM durable_job WHERE role = 'coach' AND status IN ('queued','running')
);
UPDATE durable_job
SET status = 'failed', last_error_code = 'INTERRUPTED_UPGRADE', last_error = NULL,
    completed_at = strftime('%Y-%m-%dT%H:%M:%fZ','now'), updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
WHERE role = 'coach' AND status IN ('queued','running');
UPDATE durable_job SET epoch = (SELECT epoch FROM workspace_meta WHERE singleton = 1)
WHERE epoch = '' AND run_id IS NULL;

CREATE TABLE inference_record (
  id TEXT PRIMARY KEY,
  role TEXT NOT NULL,
  status TEXT NOT NULL CHECK (status IN ('queued','running','succeeded','invalid','failed','cancelled')),
  created_at TEXT NOT NULL,
  completed_at TEXT,
  error_code TEXT,
  runner_build_digest TEXT,
  model_digest TEXT,
  prompt_digest TEXT,
  schema_digest TEXT,
  metadata_class TEXT NOT NULL CHECK (metadata_class IN ('observed','historical_unknown'))
);
INSERT INTO inference_record(id,role,status,created_at,completed_at,error_code,metadata_class)
SELECT id,role,status,created_at,completed_at,
       CASE WHEN last_error_code = 'INTERRUPTED_UPGRADE' THEN last_error_code END,
       'historical_unknown'
FROM durable_job;

-- Rebuild product references and their dependent tables before removing the
-- originals. This preserves existing cascade/restrict behavior and avoids
-- SQLite deleting delivery lineage when a referenced table is dropped.
CREATE TABLE agent_turn_ar01 (
  id TEXT PRIMARY KEY,
  chat_id TEXT NOT NULL REFERENCES agent_chat(id) ON DELETE CASCADE,
  turn_no INTEGER NOT NULL CHECK(turn_no > 0),
  speaker TEXT NOT NULL CHECK(speaker IN ('owner','agent')),
  text TEXT NOT NULL CHECK(length(trim(text)) > 0),
  job_id TEXT REFERENCES inference_record(id) ON DELETE SET NULL,
  created_at TEXT NOT NULL,
  UNIQUE(chat_id,turn_no)
);
INSERT INTO agent_turn_ar01(id,chat_id,turn_no,speaker,text,job_id,created_at)
SELECT id,chat_id,turn_no,speaker,text,job_id,created_at FROM agent_turn;

CREATE TABLE interviewer_candidate_ar01 (
  id TEXT PRIMARY KEY,
  chat_id TEXT NOT NULL REFERENCES agent_chat(id) ON DELETE CASCADE,
  job_id TEXT NOT NULL UNIQUE REFERENCES inference_record(id) ON DELETE CASCADE,
  asset_kind TEXT NOT NULL CHECK(asset_kind IN ('gift','proof','artifact')),
  fields_json TEXT NOT NULL,
  owner_quotes_json TEXT NOT NULL,
  missing_fields_json TEXT NOT NULL,
  state TEXT NOT NULL CHECK(state IN ('proposed','saved','dismissed')),
  resource_id TEXT REFERENCES entity(id) ON DELETE RESTRICT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  CHECK((state='saved' AND resource_id IS NOT NULL) OR (state<>'saved' AND resource_id IS NULL))
);
INSERT INTO interviewer_candidate_ar01(id,chat_id,job_id,asset_kind,fields_json,owner_quotes_json,missing_fields_json,state,resource_id,created_at,updated_at)
SELECT id,chat_id,job_id,asset_kind,fields_json,owner_quotes_json,missing_fields_json,state,resource_id,created_at,updated_at FROM interviewer_candidate;

CREATE TABLE brief_item_ar01 (
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
  supersedes_id TEXT REFERENCES brief_item_ar01(id) ON DELETE RESTRICT,
  job_id TEXT REFERENCES inference_record(id) ON DELETE SET NULL,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  CHECK((tier=1 AND quote IS NOT NULL AND source_start IS NOT NULL AND source_end>source_start
         AND source_occurrence_id IS NOT NULL AND source_variant_id IS NOT NULL)
     OR (tier=2 AND check_suggestion IS NOT NULL AND speaker_kind='model_prior')
     OR (tier=3 AND confirmed_by_owner=1 AND speaker_kind='owner'))
);
INSERT INTO brief_item_ar01(id,scope_kind,scope_id,kind,text,detail_json,tier,source_occurrence_id,source_variant_id,quote,source_start,source_end,check_suggestion,speaker_kind,confirmed_by_owner,status,revision,supersedes_id,job_id,created_at,updated_at)
SELECT id,scope_kind,scope_id,kind,text,detail_json,tier,source_occurrence_id,source_variant_id,quote,source_start,source_end,check_suggestion,speaker_kind,confirmed_by_owner,status,revision,supersedes_id,job_id,created_at,updated_at FROM brief_item;

CREATE TABLE coach_report_ar01 (
 id TEXT PRIMARY KEY, period TEXT NOT NULL, input_digest TEXT NOT NULL,
 event_cursor INTEGER NOT NULL, job_id TEXT NOT NULL REFERENCES inference_record(id),
 report_json TEXT NOT NULL, created_at TEXT NOT NULL, UNIQUE(period,input_digest)
);
INSERT INTO coach_report_ar01(id,period,input_digest,event_cursor,job_id,report_json,created_at)
SELECT id,period,input_digest,event_cursor,job_id,report_json,created_at FROM coach_report;

CREATE TABLE coach_question_ar01 (
 id TEXT PRIMARY KEY, job_id TEXT NOT NULL REFERENCES inference_record(id), question_key TEXT NOT NULL,
 kind TEXT NOT NULL, target_json TEXT NOT NULL, answer_schema_json TEXT NOT NULL,
 priority INTEGER NOT NULL, impact INTEGER NOT NULL, text TEXT NOT NULL,
 status TEXT NOT NULL CHECK(status IN ('open','answered','dismissed')),
 disposition_json TEXT, created_at TEXT NOT NULL, updated_at TEXT NOT NULL,
 semantic_key TEXT,
 UNIQUE(job_id,question_key)
);
INSERT INTO coach_question_ar01(id,job_id,question_key,kind,target_json,answer_schema_json,priority,impact,text,status,disposition_json,created_at,updated_at)
SELECT id,job_id,question_key,kind,target_json,answer_schema_json,priority,impact,text,status,disposition_json,created_at,updated_at FROM coach_question;

CREATE TABLE match_run_ar01 (
  id TEXT PRIMARY KEY,
  opportunity_id TEXT NOT NULL REFERENCES opportunity(id) ON DELETE RESTRICT,
  recipient_id TEXT NOT NULL REFERENCES entity(id) ON DELETE RESTRICT,
  opportunity_revision INTEGER NOT NULL,
  recipient_revision INTEGER NOT NULL,
  need_sha256 TEXT NOT NULL,
  embed_model_digest TEXT NOT NULL,
  calibration_json TEXT NOT NULL,
  shortlist_json TEXT NOT NULL,
  why_job_id TEXT REFERENCES inference_record(id) ON DELETE RESTRICT,
  created_at TEXT NOT NULL,
  FOREIGN KEY(opportunity_id,recipient_id) REFERENCES opportunity_party(opportunity_id,party_id) ON DELETE RESTRICT
);
INSERT INTO match_run_ar01(id,opportunity_id,recipient_id,opportunity_revision,recipient_revision,need_sha256,embed_model_digest,calibration_json,shortlist_json,why_job_id,created_at)
SELECT id,opportunity_id,recipient_id,opportunity_revision,recipient_revision,need_sha256,embed_model_digest,calibration_json,shortlist_json,why_job_id,created_at FROM match_run;

CREATE TABLE material_version_ar01 (
  material_id TEXT NOT NULL REFERENCES material(id) ON DELETE RESTRICT,
  version_no INTEGER NOT NULL CHECK(version_no > 0),
  opportunity_id TEXT NOT NULL,
  recipient_id TEXT NOT NULL,
  template_version_id TEXT NOT NULL REFERENCES template_version(id) ON DELETE RESTRICT,
  exchange_version INTEGER NOT NULL,
  body TEXT NOT NULL,
  slot_fills_json TEXT NOT NULL,
  sentence_bindings_json TEXT NOT NULL,
  content_sha256 TEXT NOT NULL,
  created_at TEXT NOT NULL, blob_upload_id TEXT REFERENCES blob_upload(id) ON DELETE RESTRICT, blob_sha256 TEXT REFERENCES blob(sha256) ON DELETE RESTRICT, filename TEXT, content_type TEXT, attachment_size INTEGER, job_id TEXT REFERENCES inference_record(id) ON DELETE RESTRICT, prompt_id TEXT, prompt_sha256 TEXT,
  PRIMARY KEY(material_id,version_no),
  FOREIGN KEY(material_id) REFERENCES material(id) ON DELETE RESTRICT,
  FOREIGN KEY(opportunity_id,recipient_id) REFERENCES opportunity_party(opportunity_id,party_id) ON DELETE RESTRICT
);
INSERT INTO material_version_ar01(material_id,version_no,opportunity_id,recipient_id,template_version_id,exchange_version,body,slot_fills_json,sentence_bindings_json,content_sha256,created_at,blob_upload_id,blob_sha256,filename,content_type,attachment_size,job_id,prompt_id,prompt_sha256)
SELECT material_id,version_no,opportunity_id,recipient_id,template_version_id,exchange_version,body,slot_fills_json,sentence_bindings_json,content_sha256,created_at,blob_upload_id,blob_sha256,filename,content_type,attachment_size,job_id,prompt_id,prompt_sha256 FROM material_version;

CREATE TABLE material_lint_ar01 (
  material_id TEXT NOT NULL,
  version_no INTEGER NOT NULL,
  content_sha256 TEXT NOT NULL,
  deterministic_json TEXT NOT NULL,
  advisory_json TEXT NOT NULL DEFAULT '[]',
  linter_job_id TEXT REFERENCES inference_record(id) ON DELETE RESTRICT,
  created_at TEXT NOT NULL,
  PRIMARY KEY(material_id,version_no,content_sha256),
  FOREIGN KEY(material_id,version_no) REFERENCES material_version_ar01(material_id,version_no) ON DELETE RESTRICT
);
INSERT INTO material_lint_ar01(material_id,version_no,content_sha256,deterministic_json,advisory_json,linter_job_id,created_at)
SELECT material_id,version_no,content_sha256,deterministic_json,advisory_json,linter_job_id,created_at FROM material_lint;

CREATE TABLE version_dependency_ar01 (
  material_id TEXT NOT NULL,
  version_no INTEGER NOT NULL,
  kind TEXT NOT NULL CHECK(kind IN ('resource','claim','exchange','template','party','opportunity','blob')),
  source_id TEXT NOT NULL,
  source_revision TEXT NOT NULL,
  PRIMARY KEY(material_id,version_no,kind,source_id),
  FOREIGN KEY(material_id,version_no) REFERENCES material_version_ar01(material_id,version_no) ON DELETE RESTRICT
);
INSERT INTO version_dependency_ar01(material_id,version_no,kind,source_id,source_revision)
SELECT material_id,version_no,kind,source_id,source_revision FROM version_dependency;

CREATE TABLE approval_ar01 (
  id TEXT PRIMARY KEY,
  material_id TEXT NOT NULL,
  version_no INTEGER NOT NULL,
  content_sha256 TEXT NOT NULL,
  dependency_vector_json TEXT NOT NULL,
  gate_snapshot_json TEXT NOT NULL,
  overrides_json TEXT NOT NULL,
  approved_at TEXT NOT NULL,
  UNIQUE(material_id,version_no),
  FOREIGN KEY(material_id,version_no) REFERENCES material_version_ar01(material_id,version_no) ON DELETE RESTRICT
);
INSERT INTO approval_ar01(id,material_id,version_no,content_sha256,dependency_vector_json,gate_snapshot_json,overrides_json,approved_at)
SELECT id,material_id,version_no,content_sha256,dependency_vector_json,gate_snapshot_json,overrides_json,approved_at FROM approval;

CREATE TABLE release_record_ar01 (
  id TEXT PRIMARY KEY,
  approval_id TEXT NOT NULL REFERENCES approval_ar01(id) ON DELETE RESTRICT,
  material_id TEXT NOT NULL,
  version_no INTEGER NOT NULL,
  content_sha256 TEXT NOT NULL,
  issued_at TEXT NOT NULL,
  acknowledged INTEGER CHECK(acknowledged IN (0,1)),
  acknowledged_at TEXT, bytes_issued_at TEXT, transfer_incomplete INTEGER NOT NULL DEFAULT 0 CHECK(transfer_incomplete IN (0,1)),
  FOREIGN KEY(material_id,version_no) REFERENCES material_version_ar01(material_id,version_no) ON DELETE RESTRICT
);
INSERT INTO release_record_ar01(id,approval_id,material_id,version_no,content_sha256,issued_at,acknowledged,acknowledged_at,bytes_issued_at,transfer_incomplete)
SELECT id,approval_id,material_id,version_no,content_sha256,issued_at,acknowledged,acknowledged_at,bytes_issued_at,transfer_incomplete FROM release_record;

CREATE TABLE release_ack_ar01 (
  release_id TEXT PRIMARY KEY REFERENCES release_record_ar01(id) ON DELETE RESTRICT,
  succeeded INTEGER NOT NULL CHECK(succeeded IN (0,1)),
  acknowledged_at TEXT NOT NULL
);
INSERT INTO release_ack_ar01(release_id,succeeded,acknowledged_at)
SELECT release_id,succeeded,acknowledged_at FROM release_ack;

CREATE TABLE resource_use_ar01 (
  material_id TEXT NOT NULL,
  version_no INTEGER NOT NULL,
  resource_id TEXT NOT NULL REFERENCES entity(id) ON DELETE RESTRICT,
  slot_name TEXT NOT NULL,
  PRIMARY KEY(material_id,version_no,resource_id,slot_name),
  FOREIGN KEY(material_id,version_no) REFERENCES material_version_ar01(material_id,version_no) ON DELETE RESTRICT
);
INSERT INTO resource_use_ar01(material_id,version_no,resource_id,slot_name)
SELECT material_id,version_no,resource_id,slot_name FROM resource_use;

CREATE TABLE delivered_resource_version_ar01 (
    resource_id TEXT NOT NULL REFERENCES entity(id) ON DELETE CASCADE,
    material_id TEXT NOT NULL,
    version_no INTEGER NOT NULL,
    resource_revision INTEGER NOT NULL,
    resource_title TEXT NOT NULL,
    PRIMARY KEY (resource_id, material_id, version_no),
    FOREIGN KEY (material_id, version_no) REFERENCES material_version_ar01(material_id, version_no) ON DELETE CASCADE
);
INSERT INTO delivered_resource_version_ar01(resource_id,material_id,version_no,resource_revision,resource_title)
SELECT resource_id,material_id,version_no,resource_revision,resource_title FROM delivered_resource_version;

CREATE TABLE send_part_ar01 (
  send_id TEXT NOT NULL REFERENCES send(id) ON DELETE RESTRICT,
  material_id TEXT NOT NULL REFERENCES material(id) ON DELETE RESTRICT,
  version_no INTEGER NOT NULL,
  release_id TEXT NOT NULL REFERENCES release_record_ar01(id) ON DELETE RESTRICT,
  kind TEXT NOT NULL CHECK(kind IN ('primary','companion')),
  PRIMARY KEY(send_id, material_id, version_no)
);
INSERT INTO send_part_ar01(send_id,material_id,version_no,release_id,kind)
SELECT send_id,material_id,version_no,release_id,kind FROM send_part;

CREATE TABLE proposal_batch_ar01 (
  id TEXT PRIMARY KEY,
  source_variant_id TEXT NOT NULL REFERENCES source_variant(id) ON DELETE RESTRICT,
  job_id TEXT REFERENCES inference_record(id) ON DELETE RESTRICT,
  source_occurrence_id TEXT REFERENCES source_occurrence(id) ON DELETE RESTRICT,
  status TEXT NOT NULL CHECK (status IN ('pending','partially_applied','applied','dismissed')),
  created_at TEXT NOT NULL
);
INSERT INTO proposal_batch_ar01(id,source_variant_id,job_id,status,created_at,source_occurrence_id)
SELECT id,source_variant_id,job_id,status,created_at,(SELECT source_occurrence_id FROM durable_job WHERE id=proposal_batch.job_id) FROM proposal_batch;

CREATE TABLE proposal_item_ar01 (
  id TEXT PRIMARY KEY,
  batch_id TEXT NOT NULL REFERENCES proposal_batch_ar01(id) ON DELETE CASCADE,
  parent_id TEXT REFERENCES proposal_item_ar01(id) ON DELETE RESTRICT,
  proposal_kind TEXT NOT NULL,
  candidate_json TEXT NOT NULL,
  occurrence_id TEXT REFERENCES source_occurrence(id) ON DELETE RESTRICT,
  expected_target_revision INTEGER,
  status TEXT NOT NULL CHECK (status IN ('pending','accepted','corrected','dismissed','source_only')), review_state TEXT CHECK(review_state IN ('unresolved')),
  UNIQUE(batch_id, id)
);
INSERT INTO proposal_item_ar01(id,batch_id,parent_id,proposal_kind,candidate_json,occurrence_id,expected_target_revision,status,review_state)
SELECT id,batch_id,parent_id,proposal_kind,candidate_json,occurrence_id,expected_target_revision,status,review_state FROM proposal_item;

CREATE TABLE proposal_decision_ar01 (
  id TEXT PRIMARY KEY,
  proposal_item_id TEXT NOT NULL UNIQUE REFERENCES proposal_item_ar01(id) ON DELETE CASCADE,
  decision TEXT NOT NULL CHECK(decision IN ('confirmed','corrected','dismissed','source_only','unresolved')),
  target_entity_id TEXT REFERENCES entity(id) ON DELETE RESTRICT,
  approved_wording TEXT,
  opportunity_id TEXT REFERENCES opportunity(id) ON DELETE RESTRICT,
  decided_at TEXT NOT NULL,
  CHECK((decision IN ('confirmed','corrected') AND target_entity_id IS NOT NULL AND approved_wording IS NOT NULL)
     OR decision IN ('dismissed','source_only','unresolved'))
);
INSERT INTO proposal_decision_ar01(id,proposal_item_id,decision,target_entity_id,approved_wording,opportunity_id,decided_at)
SELECT id,proposal_item_id,decision,target_entity_id,approved_wording,opportunity_id,decided_at FROM proposal_decision;

CREATE TABLE unresolved_mention_ar01 (
  id TEXT PRIMARY KEY,
  occurrence_id TEXT NOT NULL REFERENCES source_occurrence(id) ON DELETE RESTRICT,
  proposal_item_id TEXT NOT NULL UNIQUE REFERENCES proposal_item_ar01(id) ON DELETE RESTRICT,
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
INSERT INTO unresolved_mention_ar01(id,occurrence_id,proposal_item_id,mention_name,mention_kind,source_start,source_end,status,resolved_entity_id,created_at)
SELECT id,occurrence_id,proposal_item_id,mention_name,mention_kind,source_start,source_end,status,resolved_entity_id,created_at FROM unresolved_mention;

DROP TRIGGER material_version_no_update;
DROP TRIGGER material_version_no_delete;
DROP TRIGGER approval_no_update;
DROP TRIGGER approval_no_delete;
DROP TRIGGER release_no_delete;
DROP TRIGGER release_issuance_only;
DROP TRIGGER brief_scope_insert_guard;
DROP TRIGGER brief_scope_update_guard;
DROP TRIGGER material_lint_no_update;
DROP TRIGGER material_lint_no_delete;
DROP TRIGGER material_version_snapshot_resource_attachment;
DROP TABLE unresolved_mention;
DROP TABLE proposal_decision;
DROP TABLE proposal_item;
DROP TABLE proposal_batch;
DROP TABLE send_part;
DROP TABLE delivered_resource_version;
DROP TABLE resource_use;
DROP TABLE release_ack;
DROP TABLE release_record;
DROP TABLE approval;
DROP TABLE version_dependency;
DROP TABLE material_lint;
DROP TABLE material_version;
DROP TABLE match_run;
DROP TABLE coach_question;
DROP TABLE coach_report;
DROP TABLE brief_item;
DROP TABLE interviewer_candidate;
DROP TABLE agent_turn;
ALTER TABLE proposal_batch_ar01 RENAME TO proposal_batch;
ALTER TABLE proposal_item_ar01 RENAME TO proposal_item;
ALTER TABLE proposal_decision_ar01 RENAME TO proposal_decision;
ALTER TABLE unresolved_mention_ar01 RENAME TO unresolved_mention;
ALTER TABLE agent_turn_ar01 RENAME TO agent_turn;
ALTER TABLE interviewer_candidate_ar01 RENAME TO interviewer_candidate;
ALTER TABLE brief_item_ar01 RENAME TO brief_item;
ALTER TABLE coach_report_ar01 RENAME TO coach_report;
ALTER TABLE coach_question_ar01 RENAME TO coach_question;
ALTER TABLE match_run_ar01 RENAME TO match_run;
ALTER TABLE material_version_ar01 RENAME TO material_version;
ALTER TABLE material_lint_ar01 RENAME TO material_lint;
ALTER TABLE version_dependency_ar01 RENAME TO version_dependency;
ALTER TABLE approval_ar01 RENAME TO approval;
ALTER TABLE release_record_ar01 RENAME TO release_record;
ALTER TABLE release_ack_ar01 RENAME TO release_ack;
ALTER TABLE resource_use_ar01 RENAME TO resource_use;
ALTER TABLE delivered_resource_version_ar01 RENAME TO delivered_resource_version;
ALTER TABLE send_part_ar01 RENAME TO send_part;
CREATE INDEX releases_by_material ON release_record(material_id,version_no,issued_at);
CREATE TRIGGER material_version_no_update BEFORE UPDATE ON material_version
BEGIN SELECT RAISE(ABORT, 'material versions are immutable'); END;
CREATE TRIGGER material_version_no_delete BEFORE DELETE ON material_version
BEGIN SELECT RAISE(ABORT, 'material versions are immutable'); END;
CREATE TRIGGER approval_no_update BEFORE UPDATE ON approval
BEGIN SELECT RAISE(ABORT, 'approval records are immutable'); END;
CREATE TRIGGER approval_no_delete BEFORE DELETE ON approval
BEGIN SELECT RAISE(ABORT, 'approval records are immutable'); END;
CREATE TRIGGER release_no_delete BEFORE DELETE ON release_record
BEGIN SELECT RAISE(ABORT, 'release records are immutable'); END;
CREATE INDEX material_blob_upload_lookup ON material_version(blob_upload_id);
CREATE INDEX affected_versions ON version_dependency(kind,source_id,source_revision,material_id,version_no);
CREATE TRIGGER release_issuance_only BEFORE UPDATE ON release_record
WHEN NOT (NEW.id = OLD.id AND NEW.approval_id = OLD.approval_id
      AND NEW.material_id = OLD.material_id AND NEW.version_no = OLD.version_no
      AND NEW.content_sha256 = OLD.content_sha256 AND NEW.issued_at = OLD.issued_at
      AND NEW.acknowledged = OLD.acknowledged AND NEW.acknowledged_at IS OLD.acknowledged_at
      AND (NEW.bytes_issued_at IS OLD.bytes_issued_at
           OR (OLD.bytes_issued_at IS NULL AND NEW.bytes_issued_at IS NOT NULL))
      AND NEW.transfer_incomplete = OLD.transfer_incomplete)
BEGIN SELECT RAISE(ABORT, 'release issuance fields are append-only'); END;
CREATE UNIQUE INDEX release_once ON send_part(release_id);
CREATE INDEX agent_turn_chat ON agent_turn(chat_id,turn_no);
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
CREATE INDEX match_runs_by_scope ON match_run(opportunity_id,recipient_id,created_at DESC,id);
CREATE TRIGGER material_lint_no_update BEFORE UPDATE ON material_lint
BEGIN SELECT RAISE(ABORT, 'lint results are immutable for a material version'); END;
CREATE TRIGGER material_lint_no_delete BEFORE DELETE ON material_lint
BEGIN SELECT RAISE(ABORT, 'lint history is immutable'); END;
CREATE INDEX coach_report_period ON coach_report(period,created_at DESC,id DESC);
CREATE INDEX coach_question_priority ON coach_question(status,priority,impact DESC,id);
CREATE INDEX current_match_run ON match_run(opportunity_id,recipient_id,opportunity_revision,recipient_revision,created_at DESC,id);
CREATE INDEX material_included_resources ON delivered_resource_version(material_id, version_no);
CREATE INDEX send_parts_by_version ON send_part(material_id, version_no, send_id);
CREATE TRIGGER material_version_snapshot_resource_attachment
AFTER INSERT ON material_version
WHEN NEW.blob_upload_id IS NOT NULL
BEGIN
  INSERT INTO delivered_resource_version(resource_id,material_id,version_no,resource_revision,resource_title)
  SELECT e.id,NEW.material_id,NEW.version_no,e.revision,e.name
  FROM entity_attachment a JOIN entity e ON e.id=a.entity_id
  WHERE a.upload_id=NEW.blob_upload_id AND e.subtype<>'' AND e.readiness<>'retired';
END;
CREATE INDEX interviewer_candidate_chat ON interviewer_candidate(chat_id,created_at,id);

CREATE INDEX unresolved_mention_queue ON unresolved_mention(status,created_at,id);

ALTER TABLE durable_job ADD COLUMN profile_revision INTEGER REFERENCES inference_profile(revision) ON DELETE RESTRICT;
ALTER TABLE durable_job ADD COLUMN dependency_json TEXT NOT NULL DEFAULT '[]'
CHECK (json_valid(dependency_json) AND json_type(dependency_json) = 'array');
ALTER TABLE durable_job ADD COLUMN dependency_digest TEXT;
ALTER TABLE durable_job ADD COLUMN state_revision INTEGER NOT NULL DEFAULT 1 CHECK (state_revision > 0);
ALTER TABLE durable_job ADD COLUMN equivalence_key TEXT;
ALTER TABLE durable_job ADD COLUMN eligible_at TEXT;
ALTER TABLE durable_job ADD COLUMN queue_deadline_at TEXT;
ALTER TABLE durable_job ADD COLUMN total_deadline_at TEXT;
ALTER TABLE durable_job ADD COLUMN lease_id TEXT;
ALTER TABLE durable_job ADD COLUMN process_nonce TEXT;

CREATE UNIQUE INDEX inference_active_job_equivalence
ON durable_job(epoch,owner_pseudonym,role,equivalence_key)
WHERE status IN ('queued','running') AND equivalence_key IS NOT NULL;
CREATE UNIQUE INDEX inference_active_job_lease ON durable_job(lease_id) WHERE lease_id IS NOT NULL;
CREATE INDEX inference_job_ready ON durable_job(status,eligible_at,id) WHERE status = 'queued';
CREATE INDEX inference_job_expiry ON durable_job(status,queue_deadline_at,id)
WHERE status IN ('queued','running');

CREATE TABLE inference_job_dependency (
  job_id TEXT NOT NULL REFERENCES durable_job(id) ON DELETE CASCADE,
  kind TEXT NOT NULL,
  ref_id TEXT NOT NULL,
  revision TEXT NOT NULL,
  PRIMARY KEY(job_id,kind,ref_id)
);
CREATE INDEX inference_job_dependency_ref ON inference_job_dependency(kind,ref_id,job_id);

CREATE TABLE coach_question_hold (
  epoch TEXT NOT NULL,
  question_key TEXT NOT NULL,
  job_id TEXT NOT NULL REFERENCES durable_job(id) ON DELETE CASCADE,
  created_at TEXT NOT NULL,
  PRIMARY KEY(epoch,question_key)
);
CREATE INDEX coach_question_hold_job ON coach_question_hold(job_id);
CREATE UNIQUE INDEX coach_question_active_semantic
ON coach_question(semantic_key) WHERE status = 'open' AND semantic_key IS NOT NULL;

CREATE TRIGGER inference_active_identity_required
BEFORE INSERT ON durable_job
WHEN (NEW.profile_revision IS NOT NULL OR NEW.equivalence_key IS NOT NULL)
 AND (NEW.equivalence_key IS NULL OR NEW.owner_pseudonym IS NULL OR NEW.epoch = '')
BEGIN SELECT RAISE(ABORT, 'inference job requires active identity'); END;

-- Legacy role admissions are still routed through the durable job insert
-- during their own cuts. Lineage belongs to that transaction for every role.
CREATE TRIGGER inference_record_at_admission AFTER INSERT ON durable_job
BEGIN
  INSERT INTO inference_record(id,role,status,created_at,completed_at,metadata_class)
  VALUES(NEW.id,NEW.role,NEW.status,NEW.created_at,NEW.completed_at,'historical_unknown');
  UPDATE durable_job SET epoch = (SELECT epoch FROM workspace_meta WHERE singleton = 1)
  WHERE id = NEW.id AND NEW.epoch = '' AND NEW.run_id IS NULL;
END;

CREATE TRIGGER inference_record_status AFTER UPDATE OF status ON durable_job
WHEN NEW.status <> OLD.status
BEGIN
  UPDATE inference_record SET status = NEW.status,completed_at = NEW.completed_at,
      error_code = CASE WHEN NEW.status IN ('failed','invalid','cancelled') THEN NEW.last_error_code END
  WHERE id = NEW.id;
  UPDATE durable_job SET state_revision = OLD.state_revision + 1
  WHERE id = NEW.id AND NEW.state_revision = OLD.state_revision;
  DELETE FROM coach_question_hold
  WHERE job_id = NEW.id AND NEW.status IN ('succeeded','failed','invalid','cancelled');
END;

CREATE TEMP TRIGGER ar01_verify_lineage BEFORE INSERT ON ar01_preflight
WHEN EXISTS (SELECT 1 FROM pragma_foreign_key_check)
BEGIN SELECT RAISE(ABORT, 'INFERENCE_LINEAGE_ORPHAN'); END;
INSERT INTO ar01_preflight VALUES (1);
DROP TABLE ar01_preflight;
