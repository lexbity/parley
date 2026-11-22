CREATE TABLE opportunity_notes (
  opportunity_id TEXT PRIMARY KEY REFERENCES opportunity(id) ON DELETE CASCADE,
  markdown TEXT NOT NULL DEFAULT '' CHECK(length(markdown) <= 200000),
  revision INTEGER NOT NULL DEFAULT 1 CHECK(revision > 0),
  updated_at TEXT NOT NULL
);

CREATE TABLE board_state (
  singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
  revision INTEGER NOT NULL DEFAULT 1 CHECK(revision > 0)
);
INSERT INTO board_state(singleton,revision) VALUES(1,1);

CREATE TABLE opportunity_board_card (
  opportunity_id TEXT PRIMARY KEY REFERENCES opportunity(id) ON DELETE CASCADE,
  list_id TEXT NOT NULL REFERENCES board_list(id) ON DELETE RESTRICT,
  position INTEGER NOT NULL CHECK(position >= 0),
  revision INTEGER NOT NULL DEFAULT 1 CHECK(revision > 0)
);
CREATE INDEX opportunity_board_order ON opportunity_board_card(list_id,position,opportunity_id);

INSERT INTO opportunity_notes(opportunity_id,markdown,revision,updated_at)
SELECT id,'',1,updated_at FROM opportunity;
INSERT INTO opportunity_board_card(opportunity_id,list_id,position,revision)
SELECT o.id,(SELECT id FROM board_list WHERE archived=0 ORDER BY position,id LIMIT 1),
       ROW_NUMBER() OVER(ORDER BY o.created_at,o.id)-1,1
FROM opportunity o;
