CREATE TABLE maintenance_settings (
  singleton INTEGER PRIMARY KEY CHECK(singleton=1),
  backup_directory TEXT NOT NULL,
  interval_hours INTEGER NOT NULL CHECK(interval_hours BETWEEN 24 AND 168),
  retention_count INTEGER NOT NULL CHECK(retention_count BETWEEN 1 AND 90),
  revision INTEGER NOT NULL DEFAULT 1,
  last_successful_at TEXT,
  updated_at TEXT NOT NULL
);
CREATE TABLE maintenance_preview (
  id TEXT PRIMARY KEY, kind TEXT NOT NULL, subject_id TEXT, digest TEXT NOT NULL,
  domain_cursor INTEGER NOT NULL, payload_json TEXT NOT NULL, created_at TEXT NOT NULL,
  expires_at TEXT NOT NULL, consumed_at TEXT
);
CREATE TABLE maintenance_operation (
  id TEXT PRIMARY KEY, kind TEXT NOT NULL, status TEXT NOT NULL,
  subject_id TEXT, epoch TEXT NOT NULL, result_json TEXT NOT NULL,
  created_at TEXT NOT NULL, completed_at TEXT
);
CREATE TABLE erasure_tombstone(entity_id TEXT PRIMARY KEY,erased_at TEXT NOT NULL,source_generation TEXT NOT NULL);
CREATE TABLE erasure_authorization(entity_id TEXT PRIMARY KEY,subject_name TEXT NOT NULL);
CREATE TABLE legacy_key_map(
  source_format TEXT NOT NULL,
  source_table TEXT NOT NULL,
  source_key TEXT NOT NULL,
  destination_id TEXT NOT NULL,
  status TEXT NOT NULL CHECK(status IN ('converted','retained_unmapped','diagnostic')),
  diagnostic TEXT,
  PRIMARY KEY(source_format,source_table,source_key),
  UNIQUE(destination_id)
);
CREATE TABLE legacy_conversion_diagnostic(
  id INTEGER PRIMARY KEY,
  source_format TEXT NOT NULL,
  source_table TEXT NOT NULL,
  source_key TEXT,
  code TEXT NOT NULL,
  detail TEXT NOT NULL,
  created_at TEXT NOT NULL
);
DROP TRIGGER event_no_update;
CREATE TRIGGER event_no_update BEFORE UPDATE ON event
WHEN NOT (EXISTS(SELECT 1 FROM erasure_authorization WHERE entity_id = OLD.entity_id)
  AND NEW.entity_id='erased' AND NEW.payload_json='{"erased":true}')
BEGIN SELECT RAISE(ABORT,'event log is append-only'); END;
