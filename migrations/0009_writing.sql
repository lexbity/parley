ALTER TABLE agent_provider ADD COLUMN embed_model TEXT NOT NULL DEFAULT '';
ALTER TABLE agent_provider ADD COLUMN embed_model_digest TEXT;

CREATE TABLE voice_sample (
  id TEXT PRIMARY KEY,
  text TEXT NOT NULL CHECK(length(trim(text)) > 0 AND length(text) <= 20000),
  channel TEXT,
  revision INTEGER NOT NULL DEFAULT 1 CHECK(revision > 0),
  status TEXT NOT NULL DEFAULT 'active' CHECK(status IN ('active','retired')),
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);

CREATE TABLE resource_tag (
  resource_id TEXT NOT NULL REFERENCES resource(id) ON DELETE RESTRICT,
  tag TEXT NOT NULL COLLATE NOCASE CHECK(length(trim(tag)) BETWEEN 1 AND 120),
  PRIMARY KEY(resource_id,tag)
);
CREATE INDEX resource_tag_by_tag ON resource_tag(tag,resource_id);

CREATE TABLE embedding_cache (
  model_digest TEXT NOT NULL,
  text_sha256 TEXT NOT NULL,
  dimensions INTEGER NOT NULL CHECK(dimensions > 0),
  vector_json TEXT NOT NULL,
  created_at TEXT NOT NULL,
  PRIMARY KEY(model_digest,text_sha256)
);

CREATE TABLE matching_calibration (
  model_digest TEXT NOT NULL,
  asset_kind TEXT NOT NULL CHECK(asset_kind IN ('gift','proof','artifact')),
  threshold REAL NOT NULL CHECK(threshold >= -1.0 AND threshold <= 1.0),
  fixture_digest TEXT NOT NULL,
  positives INTEGER NOT NULL CHECK(positives > 0),
  negatives INTEGER NOT NULL CHECK(negatives > 0),
  true_positives INTEGER NOT NULL CHECK(true_positives >= 0),
  false_positives INTEGER NOT NULL CHECK(false_positives >= 0),
  false_negatives INTEGER NOT NULL CHECK(false_negatives >= 0),
  calibration_revision INTEGER NOT NULL CHECK(calibration_revision > 0),
  created_at TEXT NOT NULL,
  PRIMARY KEY(model_digest,asset_kind,calibration_revision)
);
CREATE INDEX active_match_calibration ON matching_calibration(model_digest,asset_kind,calibration_revision DESC);
ALTER TABLE matching_calibration ADD COLUMN scoring_config_digest TEXT NOT NULL DEFAULT '';

CREATE TABLE match_run (
  id TEXT PRIMARY KEY,
  opportunity_id TEXT NOT NULL REFERENCES opportunity(id) ON DELETE RESTRICT,
  recipient_id TEXT NOT NULL REFERENCES entity(id) ON DELETE RESTRICT,
  opportunity_revision INTEGER NOT NULL,
  recipient_revision INTEGER NOT NULL,
  need_sha256 TEXT NOT NULL,
  embed_model_digest TEXT NOT NULL,
  calibration_json TEXT NOT NULL,
  shortlist_json TEXT NOT NULL,
  why_job_id TEXT,
  created_at TEXT NOT NULL,
  FOREIGN KEY(opportunity_id,recipient_id) REFERENCES opportunity_party(opportunity_id,party_id) ON DELETE RESTRICT
);
CREATE INDEX match_runs_by_scope ON match_run(opportunity_id,recipient_id,created_at DESC,id);

ALTER TABLE template_version ADD COLUMN rules_json TEXT NOT NULL DEFAULT '[]';
ALTER TABLE template_version ADD COLUMN display_name TEXT NOT NULL DEFAULT '';
ALTER TABLE template_version ADD COLUMN stage_class TEXT NOT NULL DEFAULT 'identified';
ALTER TABLE template_version ADD COLUMN channel TEXT NOT NULL DEFAULT 'email';
ALTER TABLE template_version ADD COLUMN parent_version_id TEXT REFERENCES template_version(id) ON DELETE RESTRICT;
CREATE TRIGGER template_s09_content_immutable
BEFORE UPDATE OF family_id,version_no,body,fallbacks_json,content_sha256,created_at,rules_json,display_name,stage_class,channel,parent_version_id ON template_version
BEGIN SELECT RAISE(ABORT, 'template version content is immutable'); END;

ALTER TABLE material_version ADD COLUMN job_id TEXT REFERENCES durable_job(id) ON DELETE RESTRICT;
ALTER TABLE material_version ADD COLUMN prompt_id TEXT;
ALTER TABLE material_version ADD COLUMN prompt_sha256 TEXT;

CREATE TABLE material_lint (
  material_id TEXT NOT NULL,
  version_no INTEGER NOT NULL,
  content_sha256 TEXT NOT NULL,
  deterministic_json TEXT NOT NULL,
  advisory_json TEXT NOT NULL DEFAULT '[]',
  linter_job_id TEXT REFERENCES durable_job(id) ON DELETE RESTRICT,
  created_at TEXT NOT NULL,
  PRIMARY KEY(material_id,version_no,content_sha256),
  FOREIGN KEY(material_id,version_no) REFERENCES material_version(material_id,version_no) ON DELETE RESTRICT
);
CREATE TRIGGER material_lint_no_update BEFORE UPDATE ON material_lint
BEGIN SELECT RAISE(ABORT, 'lint results are immutable for a material version'); END;
CREATE TRIGGER material_lint_no_delete BEFORE DELETE ON material_lint
BEGIN SELECT RAISE(ABORT, 'lint history is immutable'); END;
