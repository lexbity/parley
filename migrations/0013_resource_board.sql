-- R01: Resource board (kanban) for owner-managed resources.
--
-- Resources are entity rows (kind='resource') with typed subtype data,
-- canonical Markdown notes and attachment bindings, so the board card, graph
-- node and inspector share one ID (amendment §6). The board itself is fixed
-- columns plus a unique placement per active resource; positions are spaced
-- ranks with a revision per placement and per column. Legacy `resource`
-- rows stay untouched and off board, discoverable through the resource
-- picker (amendment §2, §7).

ALTER TABLE entity ADD COLUMN subtype TEXT NOT NULL DEFAULT ''
  CHECK(subtype IN ('','gift','proof','artifact','skill','interest','note'));
ALTER TABLE entity ADD COLUMN body_json TEXT NOT NULL DEFAULT '{}';
ALTER TABLE entity ADD COLUMN readiness TEXT NOT NULL DEFAULT 'draft'
  CHECK(readiness IN ('draft','ready','retired'));

-- Owner-authored edges keep the provenance the inspector shows: the basis
-- text and author kind, independent of source-attributed claim evidence.
ALTER TABLE edge ADD COLUMN basis TEXT NOT NULL DEFAULT '';
ALTER TABLE edge ADD COLUMN author_kind TEXT NOT NULL DEFAULT 'owner'
  CHECK(author_kind IN ('owner','party'));

CREATE TABLE resource_column (
  id TEXT PRIMARY KEY,
  title TEXT NOT NULL CHECK(length(title) BETWEEN 1 AND 100),
  position INTEGER NOT NULL,
  revision INTEGER NOT NULL DEFAULT 1 CHECK(revision > 0),
  kind TEXT NOT NULL DEFAULT 'owner' CHECK(kind IN ('owner','system'))
);

CREATE TABLE resource_placement (
  resource_id TEXT PRIMARY KEY REFERENCES entity(id) ON DELETE CASCADE,
  column_id TEXT NOT NULL REFERENCES resource_column(id) ON DELETE RESTRICT,
  position INTEGER NOT NULL,
  revision INTEGER NOT NULL DEFAULT 1 CHECK(revision > 0),
  UNIQUE(column_id, position)
);
CREATE INDEX resource_placement_order ON resource_placement(column_id, position, resource_id);

CREATE TRIGGER resource_placement_insert_guard
BEFORE INSERT ON resource_placement
WHEN NOT EXISTS (SELECT 1 FROM entity WHERE id=NEW.resource_id AND kind='resource')
BEGIN SELECT RAISE(ABORT, 'placement requires a resource entity'); END;

CREATE TRIGGER resource_placement_update_guard
BEFORE UPDATE OF resource_id ON resource_placement
WHEN NOT EXISTS (SELECT 1 FROM entity WHERE id=NEW.resource_id AND kind='resource')
BEGIN SELECT RAISE(ABORT, 'placement requires a resource entity'); END;

-- Seed the fixed owner columns once per workspace. Stable IDs are
-- independent of display labels (amendment §6).
INSERT INTO resource_column(id,title,position,revision,kind) VALUES
 ('inbox','Inbox',1024,1,'system'),
 ('developing','Developing',2048,1,'system'),
 ('available','Available',3072,1,'system');
