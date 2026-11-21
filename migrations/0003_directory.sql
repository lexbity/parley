CREATE TABLE entity_detail (
  entity_id TEXT PRIMARY KEY REFERENCES entity(id) ON DELETE CASCADE,
  descriptor TEXT NOT NULL DEFAULT '',
  owner_notes TEXT NOT NULL DEFAULT '',
  revision INTEGER NOT NULL DEFAULT 1 CHECK (revision > 0)
);
CREATE TABLE entity_alias (
  entity_id TEXT NOT NULL REFERENCES entity(id) ON DELETE CASCADE,
  alias TEXT NOT NULL,
  normalized_alias TEXT NOT NULL COLLATE NOCASE,
  PRIMARY KEY(entity_id, normalized_alias)
);
CREATE INDEX entity_alias_lookup ON entity_alias(normalized_alias, entity_id);
CREATE TABLE entity_tag (
  entity_id TEXT NOT NULL REFERENCES entity(id) ON DELETE CASCADE,
  tag TEXT NOT NULL,
  normalized_tag TEXT NOT NULL COLLATE NOCASE,
  PRIMARY KEY(entity_id, normalized_tag)
);
CREATE INDEX entity_tag_lookup ON entity_tag(normalized_tag, entity_id);
INSERT INTO entity_detail(entity_id)
SELECT id FROM entity;
