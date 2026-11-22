CREATE TABLE agent_prompt_version_s10 (
 role TEXT NOT NULL CHECK(role IN ('interviewer','briefer','coach')),
 version_no INTEGER NOT NULL CHECK(version_no>0),version_id TEXT NOT NULL UNIQUE,prompt_text TEXT NOT NULL,
 prompt_sha256 TEXT NOT NULL,created_by TEXT NOT NULL CHECK(created_by IN ('bundled','owner')),created_at TEXT NOT NULL,PRIMARY KEY(role,version_no));
INSERT INTO agent_prompt_version_s10 SELECT * FROM agent_prompt_version;
CREATE TABLE agent_prompt_active_s10 (role TEXT PRIMARY KEY,version_id TEXT NOT NULL UNIQUE REFERENCES agent_prompt_version_s10(version_id));
INSERT INTO agent_prompt_active_s10 SELECT * FROM agent_prompt_active;
DROP TABLE agent_prompt_active;
DROP TABLE agent_prompt_version;
ALTER TABLE agent_prompt_version_s10 RENAME TO agent_prompt_version;
ALTER TABLE agent_prompt_active_s10 RENAME TO agent_prompt_active;

CREATE TABLE coach_report (
 id TEXT PRIMARY KEY, period TEXT NOT NULL, input_digest TEXT NOT NULL,
 event_cursor INTEGER NOT NULL, job_id TEXT NOT NULL REFERENCES durable_job(id),
 report_json TEXT NOT NULL, created_at TEXT NOT NULL, UNIQUE(period,input_digest)
);
CREATE INDEX coach_report_period ON coach_report(period,created_at DESC,id DESC);
CREATE TABLE coach_question (
 id TEXT PRIMARY KEY, job_id TEXT NOT NULL REFERENCES durable_job(id), question_key TEXT NOT NULL,
 kind TEXT NOT NULL, target_json TEXT NOT NULL, answer_schema_json TEXT NOT NULL,
 priority INTEGER NOT NULL, impact INTEGER NOT NULL, text TEXT NOT NULL,
 status TEXT NOT NULL CHECK(status IN ('open','answered','dismissed')),
 disposition_json TEXT, created_at TEXT NOT NULL, updated_at TEXT NOT NULL,
 UNIQUE(job_id,question_key)
);
CREATE INDEX coach_question_priority ON coach_question(status,priority,impact DESC,id);
CREATE TABLE outcome_revision (
 send_id TEXT NOT NULL REFERENCES outcome(send_id), revision INTEGER NOT NULL,
 class TEXT NOT NULL, rung_accepted INTEGER, received_at TEXT, recorded_at TEXT NOT NULL,
 note TEXT, correction_reason TEXT, PRIMARY KEY(send_id,revision),
 CHECK((revision=1 AND correction_reason IS NULL) OR (revision>1 AND length(trim(correction_reason))>=10))
);
INSERT INTO outcome_revision(send_id,revision,class,rung_accepted,received_at,recorded_at,note,correction_reason)
 SELECT send_id,1,class,rung_accepted,received_at,recorded_at,note,NULL FROM outcome;
CREATE TRIGGER outcome_revision_seed AFTER INSERT ON outcome BEGIN
 INSERT INTO outcome_revision(send_id,revision,class,rung_accepted,received_at,recorded_at,note,correction_reason)
 VALUES(NEW.send_id,1,NEW.class,NEW.rung_accepted,NEW.received_at,NEW.recorded_at,NEW.note,NULL);
END;
CREATE TRIGGER outcome_revision_no_update BEFORE UPDATE ON outcome_revision BEGIN SELECT RAISE(ABORT,'outcome revisions are append-only'); END;
CREATE TRIGGER outcome_revision_no_delete BEFORE DELETE ON outcome_revision BEGIN SELECT RAISE(ABORT,'outcome revisions are append-only'); END;
CREATE TABLE opportunity_tag (opportunity_id TEXT NOT NULL REFERENCES opportunity(id) ON DELETE CASCADE,tag TEXT NOT NULL COLLATE NOCASE,PRIMARY KEY(opportunity_id,tag));
CREATE TABLE accepted_hook (claim_id TEXT NOT NULL,claim_revision INTEGER NOT NULL,published_at TEXT NOT NULL,publicness TEXT NOT NULL CHECK(publicness='professional_public'),accepted_at TEXT NOT NULL,PRIMARY KEY(claim_id,claim_revision),FOREIGN KEY(claim_id,claim_revision) REFERENCES claim_revision(claim_id,revision));
CREATE TABLE owner_identity_tag (tag TEXT PRIMARY KEY COLLATE NOCASE,confirmed_at TEXT NOT NULL);
CREATE TABLE owner_identity_revision (id INTEGER PRIMARY KEY CHECK(id=1), revision INTEGER NOT NULL CHECK(revision>0));
INSERT INTO owner_identity_revision(id,revision) VALUES(1,1);
CREATE TABLE party_identity_tag (entity_id TEXT NOT NULL REFERENCES entity(id),tag TEXT NOT NULL COLLATE NOCASE,claim_id TEXT NOT NULL,claim_revision INTEGER NOT NULL,created_at TEXT NOT NULL,PRIMARY KEY(entity_id,tag),FOREIGN KEY(claim_id,claim_revision) REFERENCES claim_revision(claim_id,revision));
ALTER TABLE send ADD COLUMN segment_ids_snapshot_json TEXT NOT NULL DEFAULT '[]';
CREATE INDEX current_match_run ON match_run(opportunity_id,recipient_id,opportunity_revision,recipient_revision,created_at DESC,id);
