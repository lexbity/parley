-- UCR-001 U06: canonical resources — `entity` is the sole resource record.
--
-- Migration 0020 made `entity` the board/graph record while the legacy
-- `resource` table stayed mirror-synced. This migration converges the two:
--
--   * `entity` gains a separate content revision
--     (`resource_content_revision`) and an explicit subtype-lock flag, so a
--     board move or notes edit never disturbs an approved material's content
--     proof;
--   * the three tables that referenced the legacy row (`resource_use`,
--     `resource_tag`, `interviewer_candidate`) rebuild their foreign keys
--     onto `entity`;
--   * the graph projection stops joining the legacy table and the mirror
--     triggers are dropped;
--   * the legacy `resource` table is dropped after a preflight that repairs
--     the documented empty-subtype state and refuses divergent populated
--     mirrors.
--
-- IDs, card/notes revisions, historical hashes, material bytes, captured
-- titles/revisions and exact upload provenance are preserved.

-- 1. Content revision and subtype-lock state.
ALTER TABLE entity ADD COLUMN resource_content_revision INTEGER NOT NULL DEFAULT 1
  CHECK(resource_content_revision > 0);
ALTER TABLE entity ADD COLUMN subtype_locked INTEGER NOT NULL DEFAULT 0
  CHECK(subtype_locked IN (0,1));

-- Repair the documented 0018 gap deterministically: an entity that mirrors a
-- legacy resource but still has the empty subtype takes the legacy kind,
-- matching what the 0020 backfill did for the rows it saw.
UPDATE entity
SET subtype = (SELECT r.kind FROM resource r WHERE r.id = entity.id)
WHERE subtype = '' AND EXISTS (SELECT 1 FROM resource r WHERE r.id = entity.id);

-- Preflight: a populated entity name, body or readiness that disagrees with
-- its legacy twin means the mirrors diverged and history cannot be trusted.
-- The temporary table trigger aborts the migration when a divergence exists.
CREATE TABLE _ucr006_preflight (ok INTEGER);
CREATE TRIGGER _ucr006_preflight_fail BEFORE INSERT ON _ucr006_preflight
WHEN EXISTS (
  SELECT 1
  FROM entity e JOIN resource r ON r.id = e.id
  WHERE (e.name <> '' AND e.name <> r.title)
     OR (e.body_json <> r.body_json)
     OR (e.readiness <> r.readiness)
)
BEGIN
  SELECT RAISE(ABORT, 'canonical resource preflight: entity and legacy resource diverge');
END;
INSERT INTO _ucr006_preflight VALUES (1);
DROP TABLE _ucr006_preflight;

-- Legacy resources were renamed through the entity PATCH path, which kept the
-- entity name empty because the legacy row owned the title; restore the
-- canonical name before the legacy table goes away.
UPDATE entity
SET name = (SELECT r.title FROM resource r WHERE r.id = entity.id)
WHERE subtype <> '' AND name = '' AND EXISTS (SELECT 1 FROM resource r WHERE r.id = entity.id);

-- 2. Backfill content revisions. Legacy resources keep the exact revision
-- their material dependencies were bound against; entity-only resources keep
-- their card revision as the content revision at convergence.
UPDATE entity
SET resource_content_revision = (SELECT r.revision FROM resource r WHERE r.id = entity.id)
WHERE EXISTS (SELECT 1 FROM resource r WHERE r.id = entity.id);

UPDATE entity
SET resource_content_revision = revision
WHERE subtype <> ''
  AND resource_content_revision = 1
  AND NOT EXISTS (SELECT 1 FROM resource r WHERE r.id = entity.id);

-- 3. Subtype lock backfill: a migrated resource could never change subtype
-- once it had typed data or a ready status; persist that lock on entity.
UPDATE entity
SET subtype_locked = 1
WHERE subtype <> ''
  AND (EXISTS (SELECT 1 FROM resource r WHERE r.id = entity.id)
       OR body_json <> '{}' OR readiness = 'ready');

-- 4. Rebuild reference tables onto entity. SQLite renames update no foreign
-- key targets, so each reference table is recreated, copied, dropped and
-- renamed with its indexes.
DROP INDEX IF EXISTS interviewer_candidate_chat;
CREATE TABLE interviewer_candidate_new (
  id TEXT PRIMARY KEY,
  chat_id TEXT NOT NULL REFERENCES agent_chat(id) ON DELETE CASCADE,
  job_id TEXT NOT NULL UNIQUE REFERENCES durable_job(id) ON DELETE CASCADE,
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
INSERT INTO interviewer_candidate_new
SELECT id,chat_id,job_id,asset_kind,fields_json,owner_quotes_json,missing_fields_json,state,resource_id,created_at,updated_at
FROM interviewer_candidate;
DROP TABLE interviewer_candidate;
ALTER TABLE interviewer_candidate_new RENAME TO interviewer_candidate;
CREATE INDEX interviewer_candidate_chat ON interviewer_candidate(chat_id,created_at,id);

CREATE TABLE resource_tag_new (
  resource_id TEXT NOT NULL REFERENCES entity(id) ON DELETE RESTRICT,
  tag TEXT NOT NULL COLLATE NOCASE CHECK(length(trim(tag)) BETWEEN 1 AND 120),
  PRIMARY KEY(resource_id,tag)
);
INSERT INTO resource_tag_new SELECT resource_id,tag FROM resource_tag;
DROP TABLE resource_tag;
ALTER TABLE resource_tag_new RENAME TO resource_tag;
CREATE INDEX resource_tag_by_tag ON resource_tag(tag,resource_id);

CREATE TABLE resource_use_new (
  material_id TEXT NOT NULL,
  version_no INTEGER NOT NULL,
  resource_id TEXT NOT NULL REFERENCES entity(id) ON DELETE RESTRICT,
  slot_name TEXT NOT NULL,
  PRIMARY KEY(material_id,version_no,resource_id,slot_name),
  FOREIGN KEY(material_id,version_no) REFERENCES material_version(material_id,version_no) ON DELETE RESTRICT
);
INSERT INTO resource_use_new SELECT material_id,version_no,resource_id,slot_name FROM resource_use;
DROP TABLE resource_use;
ALTER TABLE resource_use_new RENAME TO resource_use;

-- 5. Drop the mirror triggers, redefine the graph projection from entity,
-- and drop the legacy table.
DROP TRIGGER resource_create_graph_card;
DROP TRIGGER resource_update_graph_card;

DROP VIEW graph_card;
CREATE VIEW graph_card AS
SELECT e.id,e.kind,e.name,e.focus,e.revision
FROM entity e
WHERE e.retired_at IS NULL AND e.readiness<>'retired';

DROP TABLE resource;
