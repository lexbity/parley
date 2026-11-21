ALTER TABLE opportunity ADD COLUMN notes_markdown TEXT NOT NULL DEFAULT '';
ALTER TABLE opportunity ADD COLUMN notes_revision INTEGER NOT NULL DEFAULT 1 CHECK(notes_revision > 0);
ALTER TABLE opportunity ADD COLUMN notes_next_task TEXT;

CREATE TABLE board_placement (
  opportunity_id TEXT PRIMARY KEY REFERENCES opportunity(id) ON DELETE CASCADE,
  list_id TEXT NOT NULL REFERENCES board_list(id) ON DELETE RESTRICT,
  position INTEGER NOT NULL,
  revision INTEGER NOT NULL DEFAULT 1 CHECK(revision > 0),
  UNIQUE(list_id,position)
);
CREATE INDEX board_placement_order ON board_placement(list_id,position,opportunity_id);

CREATE TRIGGER board_placement_insert_guard
BEFORE INSERT ON board_placement
WHEN NOT EXISTS (SELECT 1 FROM board_list WHERE id=NEW.list_id AND archived=0)
BEGIN SELECT RAISE(ABORT, 'placement requires an active board list'); END;
CREATE TRIGGER board_placement_update_guard
BEFORE UPDATE OF list_id ON board_placement
WHEN NOT EXISTS (SELECT 1 FROM board_list WHERE id=NEW.list_id AND archived=0)
BEGIN SELECT RAISE(ABORT, 'placement requires an active board list'); END;
CREATE TRIGGER board_list_archive_guard
BEFORE UPDATE OF archived ON board_list
WHEN OLD.archived=0 AND NEW.archived=1
  AND (EXISTS (SELECT 1 FROM board_placement WHERE list_id=OLD.id)
       OR (SELECT count(*) FROM board_list WHERE archived=0) <= 1)
BEGIN SELECT RAISE(ABORT, 'cannot archive a nonempty list or the last active list'); END;

INSERT INTO board_placement(opportunity_id,list_id,position,revision)
SELECT id,'inbox',ROW_NUMBER() OVER (ORDER BY created_at,id)*1024,1
FROM opportunity;
