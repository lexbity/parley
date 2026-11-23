-- P01/P03: Party sheet, entity notes and card attachment bindings.
--
-- One SQLite-backed entity ID serves the sheet row, graph node and inspector
-- (amendment §5). Focus is the owner-editable short descriptor shown on the
-- sheet; entity notes are independent, revisioned Markdown. File bindings
-- reference the verified blob upload records (0006) so byte identity is
-- shared while each binding keeps its own provenance and owner permission.

ALTER TABLE entity ADD COLUMN focus TEXT NOT NULL DEFAULT '';
ALTER TABLE entity ADD COLUMN notes_md TEXT NOT NULL DEFAULT '';
ALTER TABLE entity ADD COLUMN notes_rev INTEGER NOT NULL DEFAULT 1 CHECK(notes_rev > 0);

CREATE TABLE entity_attachment (
  id TEXT PRIMARY KEY,
  entity_id TEXT NOT NULL REFERENCES entity(id) ON DELETE RESTRICT,
  upload_id TEXT NOT NULL UNIQUE REFERENCES blob_upload(id) ON DELETE RESTRICT,
  filename TEXT NOT NULL CHECK(length(filename) BETWEEN 1 AND 500),
  content_type TEXT NOT NULL CHECK(length(content_type) BETWEEN 1 AND 200),
  size INTEGER NOT NULL CHECK(size > 0),
  sha256 TEXT NOT NULL CHECK(length(sha256) = 64),
  created_at TEXT NOT NULL
);
CREATE INDEX entity_attachment_by_entity ON entity_attachment(entity_id, created_at);
