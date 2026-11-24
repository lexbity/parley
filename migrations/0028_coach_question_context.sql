-- AR02: scoped Coach identity, dependency fencing, revisioned dispositions and defer.
DROP INDEX coach_question_active_semantic;
DROP INDEX coach_question_priority;

CREATE TABLE coach_question_ar02 (
  id TEXT PRIMARY KEY,
  job_id TEXT NOT NULL REFERENCES inference_record(id),
  question_key TEXT NOT NULL,
  semantic_key TEXT,
  kind TEXT NOT NULL,
  target_json TEXT NOT NULL CHECK(json_valid(target_json)),
  selector_json TEXT NOT NULL DEFAULT '{"kind":"weekly"}' CHECK(json_valid(selector_json)),
  dependency_json TEXT NOT NULL DEFAULT '[]' CHECK(json_valid(dependency_json) AND json_type(dependency_json)='array'),
  dependency_digest TEXT,
  suggested_resource_id TEXT REFERENCES entity(id) ON DELETE SET NULL,
  answer_schema_json TEXT NOT NULL CHECK(json_valid(answer_schema_json)),
  priority INTEGER NOT NULL,
  impact INTEGER NOT NULL,
  text TEXT NOT NULL,
  status TEXT NOT NULL CHECK(status IN ('open','answer_pending','answered','dismissed','deferred','superseded')),
  disposition_json TEXT CHECK(disposition_json IS NULL OR json_valid(disposition_json)),
  revision INTEGER NOT NULL DEFAULT 1 CHECK(revision > 0),
  deferred_until TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  UNIQUE(job_id,question_key)
);
INSERT INTO coach_question_ar02(
  id,job_id,question_key,semantic_key,kind,target_json,selector_json,dependency_json,
  dependency_digest,suggested_resource_id,answer_schema_json,priority,impact,text,status,disposition_json,
  revision,deferred_until,created_at,updated_at)
SELECT id,job_id,question_key,semantic_key,kind,target_json,
  json_object('kind','weekly','period',coalesce((SELECT scope_id FROM durable_job WHERE id=coach_question.job_id),'')),
  '[]',NULL,NULL,answer_schema_json,priority,impact,text,status,disposition_json,1,NULL,created_at,updated_at
FROM coach_question;
DROP TABLE coach_question;
ALTER TABLE coach_question_ar02 RENAME TO coach_question;

-- Retain the oldest actionable wording for an already duplicated semantic
-- key. Later rows keep their historical text but are no longer actionable.
UPDATE coach_question
SET status='superseded',
    disposition_json=json_object('kind','migration_duplicate','retained_id',(
      SELECT q2.id FROM coach_question q2 WHERE q2.semantic_key=coach_question.semantic_key
        AND q2.status IN ('open','answer_pending') ORDER BY q2.created_at,q2.id LIMIT 1
    )),
    revision=revision+1
WHERE status IN ('open','answer_pending') AND semantic_key IS NOT NULL
  AND id<>(SELECT q2.id FROM coach_question q2 WHERE q2.semantic_key=coach_question.semantic_key
    AND q2.status IN ('open','answer_pending') ORDER BY q2.created_at,q2.id LIMIT 1);

CREATE INDEX coach_question_priority ON coach_question(status,priority,impact DESC,id);
CREATE INDEX coach_question_selector ON coach_question(status,selector_json,updated_at DESC,id);
CREATE INDEX coach_question_dependency ON coach_question(dependency_digest,id);
CREATE UNIQUE INDEX coach_question_active_semantic
ON coach_question(semantic_key) WHERE status IN ('open','answer_pending') AND semantic_key IS NOT NULL;
CREATE INDEX coach_question_deferred ON coach_question(status,deferred_until,id) WHERE status='deferred';

CREATE TRIGGER coach_question_revision_monotonic
BEFORE UPDATE OF status,disposition_json,selector_json,target_json,deferred_until,dependency_json,semantic_key ON coach_question
WHEN NEW.revision <> OLD.revision + 1
BEGIN SELECT RAISE(ABORT,'coach question revision must advance by one'); END;
