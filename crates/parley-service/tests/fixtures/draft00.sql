PRAGMA foreign_keys=ON;
CREATE TABLE owner_profile (
  id INTEGER PRIMARY KEY CHECK(id=1), display_name TEXT NOT NULL,
  pack_id TEXT NOT NULL, pack_version TEXT NOT NULL,
  timezone TEXT NOT NULL, settings_json TEXT NOT NULL DEFAULT '{}'
);
INSERT INTO owner_profile VALUES(1,'Legacy Owner','job_search','1.0.0','UTC','{}');
CREATE TABLE party (
  id INTEGER PRIMARY KEY, kind TEXT NOT NULL, name TEXT NOT NULL,
  stage TEXT NOT NULL, stage_class TEXT NOT NULL,
  highest_rung INTEGER NOT NULL DEFAULT 0, touches INTEGER NOT NULL DEFAULT 0,
  do_not_contact INTEGER NOT NULL DEFAULT 0, created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);
INSERT INTO party VALUES(42,'person','Dana Lee','Contacted','touched',2,1,1,'2026-11-20T12:00:00Z','2026-11-20T12:00:00Z');
CREATE TABLE capture (
  id INTEGER PRIMARY KEY, party_id INTEGER, url TEXT, title TEXT, text TEXT,
  text_sha256 TEXT NOT NULL, norm_version INTEGER NOT NULL, method TEXT NOT NULL,
  captured_at TEXT NOT NULL
);
INSERT INTO capture VALUES(7,42,'https://example.invalid','Hiring update','The next engineer will work with Dana.','fixture-sha',1,'paste','2026-11-20T12:00:00Z');
CREATE INDEX idx_legacy_party_name ON party(name);
