-- AR-001 AR03: contextual Template Builder candidates with separate preview
-- storage. A candidate is a validated reusable skeleton plus a distinct typed
-- preview binding for the selected party/opportunity context. Product lineage
-- (template_version drafts) stays in the portable tables; these pending
-- proposals are transient and omit private preview values from backups.
PRAGMA defer_foreign_keys = ON;

CREATE TABLE template_build_candidate (
  id TEXT PRIMARY KEY,
  job_id TEXT NOT NULL REFERENCES inference_record(id) ON DELETE RESTRICT,
  variant_no INTEGER NOT NULL CHECK(variant_no BETWEEN 1 AND 3),
  name TEXT NOT NULL CHECK(length(trim(name)) BETWEEN 1 AND 200),
  body TEXT NOT NULL CHECK(length(body) BETWEEN 1 AND 10000),
  sentences_json TEXT NOT NULL CHECK(json_valid(sentences_json) AND json_type(sentences_json) = 'array'),
  fallbacks_json TEXT NOT NULL DEFAULT '{}' CHECK(json_valid(fallbacks_json) AND json_type(fallbacks_json) = 'object'),
  notes TEXT NOT NULL DEFAULT '' CHECK(length(notes) <= 2000),
  requested_rules_json TEXT NOT NULL CHECK(json_valid(requested_rules_json) AND json_type(requested_rules_json) = 'array'),
  optional_rules_json TEXT NOT NULL DEFAULT '[]' CHECK(json_valid(optional_rules_json) AND json_type(optional_rules_json) = 'array'),
  dependency_json TEXT NOT NULL CHECK(json_valid(dependency_json) AND json_type(dependency_json) = 'array'),
  dependency_digest TEXT NOT NULL,
  preview_json TEXT NOT NULL CHECK(json_valid(preview_json) AND json_type(preview_json) = 'object'),
  status TEXT NOT NULL DEFAULT 'owner_review_required' CHECK(status IN ('owner_review_required','saved','expired')),
  revision INTEGER NOT NULL DEFAULT 1 CHECK(revision > 0),
  saved_version_id TEXT REFERENCES template_version(id) ON DELETE RESTRICT,
  family_head_id TEXT REFERENCES template_version(id) ON DELETE SET NULL,
  expires_at TEXT NOT NULL,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  UNIQUE(job_id, variant_no),
  CHECK((status = 'saved') = (saved_version_id IS NOT NULL)),
  CHECK(status <> 'expired' OR saved_version_id IS NULL)
);

-- One immutable draft may be saved from each candidate. The same intent
-- replays; a differing edited save is normal authoring of a new draft.
CREATE UNIQUE INDEX template_build_candidate_saved_once
ON template_build_candidate(saved_version_id) WHERE saved_version_id IS NOT NULL;

CREATE INDEX template_build_candidate_job
ON template_build_candidate(job_id, variant_no);

CREATE INDEX template_build_candidate_dependency
ON template_build_candidate(dependency_digest, id);

CREATE INDEX template_build_candidate_expiry
ON template_build_candidate(status, expires_at, id) WHERE status = 'owner_review_required';

CREATE TRIGGER template_build_candidate_revision_monotonic
BEFORE UPDATE OF status,saved_version_id,preview_json ON template_build_candidate
WHEN NEW.revision <> OLD.revision + 1
BEGIN SELECT RAISE(ABORT, 'template build candidate revision must advance by one'); END;
