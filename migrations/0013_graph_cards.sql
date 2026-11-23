-- no-transaction
-- SQLite cannot widen a CHECK in place. Keep endpoint IDs and the foreign
-- keys in all referencing tables intact while replacing the entity table.
PRAGMA foreign_keys = OFF;
PRAGMA legacy_alter_table = ON;
BEGIN IMMEDIATE;
CREATE TABLE expanded_entity (
  id TEXT PRIMARY KEY,
  kind TEXT NOT NULL CHECK(kind IN ('person','org','project','resource','source','interest','artifact')),
  name TEXT NOT NULL,
  revision INTEGER NOT NULL CHECK(revision > 0),
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  focus TEXT NOT NULL DEFAULT '',
  notes_md TEXT NOT NULL DEFAULT '',
  notes_rev INTEGER NOT NULL DEFAULT 1 CHECK(notes_rev > 0)
);
INSERT INTO expanded_entity SELECT id,kind,name,revision,created_at,updated_at,focus,notes_md,notes_rev FROM entity;
DROP TABLE entity;
ALTER TABLE expanded_entity RENAME TO entity;
CREATE INDEX entity_name ON entity(name COLLATE NOCASE, id);
CREATE TABLE IF NOT EXISTS edge_history (
  edge_id TEXT NOT NULL REFERENCES edge(id),
  revision INTEGER NOT NULL,
  from_id TEXT NOT NULL,
  to_id TEXT NOT NULL,
  verb TEXT NOT NULL,
  state TEXT NOT NULL,
  evidence_json TEXT NOT NULL,
  recorded_at TEXT NOT NULL,
  PRIMARY KEY(edge_id,revision)
);
COMMIT;
PRAGMA legacy_alter_table = OFF;
PRAGMA foreign_keys = ON;
