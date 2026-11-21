PRAGMA foreign_keys = ON;
CREATE TABLE workspace_meta (
  singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
  schema_version INTEGER NOT NULL,
  epoch TEXT NOT NULL,
  revision INTEGER NOT NULL DEFAULT 0 CHECK (revision >= 0),
  created_at TEXT NOT NULL
);
CREATE TABLE owner_profile (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  display_name TEXT NOT NULL,
  timezone TEXT NOT NULL,
  pack_id TEXT NOT NULL,
  pack_version TEXT NOT NULL,
  settings_json TEXT NOT NULL DEFAULT '{}'
);
CREATE TABLE resource (
  id TEXT PRIMARY KEY,
  kind TEXT NOT NULL CHECK (kind IN ('gift','proof','artifact')),
  title TEXT NOT NULL,
  body_json TEXT NOT NULL,
  readiness TEXT NOT NULL CHECK (readiness IN ('draft','ready','retired')),
  revision INTEGER NOT NULL CHECK (revision > 0),
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);
CREATE TABLE board_list (
  id TEXT PRIMARY KEY,
  title TEXT NOT NULL,
  position INTEGER NOT NULL,
  archived INTEGER NOT NULL DEFAULT 0 CHECK (archived IN (0,1)),
  revision INTEGER NOT NULL DEFAULT 1 CHECK (revision > 0)
);
CREATE TABLE idempotency_receipt (
  owner_id INTEGER NOT NULL DEFAULT 1 REFERENCES owner_profile(id),
  epoch TEXT NOT NULL,
  key TEXT NOT NULL,
  canonical_digest TEXT NOT NULL,
  status_code INTEGER NOT NULL,
  result_json TEXT NOT NULL,
  last_event_cursor INTEGER NOT NULL,
  created_at TEXT NOT NULL,
  PRIMARY KEY (owner_id, epoch, key)
);
CREATE TABLE event (
  cursor INTEGER PRIMARY KEY AUTOINCREMENT,
  event_id TEXT NOT NULL UNIQUE,
  occurred_at TEXT NOT NULL,
  entity_type TEXT NOT NULL,
  entity_id TEXT NOT NULL,
  revision INTEGER NOT NULL,
  event_type TEXT NOT NULL,
  payload_json TEXT NOT NULL
);
CREATE TRIGGER event_no_update BEFORE UPDATE ON event
BEGIN SELECT RAISE(ABORT, 'event log is append-only'); END;
CREATE TRIGGER event_no_delete BEFORE DELETE ON event
BEGIN SELECT RAISE(ABORT, 'event log is append-only'); END;
INSERT INTO workspace_meta(singleton,schema_version,epoch,revision,created_at)
VALUES(1,1,'initial',0,strftime('%Y-%m-%dT%H:%M:%fZ','now'));
INSERT INTO owner_profile(id,display_name,timezone,pack_id,pack_version,settings_json)
VALUES(1,'Owner','UTC','job_search','1.0.0','{}');
INSERT INTO board_list(id,title,position) VALUES('inbox','Inbox',1024);
