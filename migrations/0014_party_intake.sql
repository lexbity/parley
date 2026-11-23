-- Raw input is private, transient working data. Successful commit clears it.
CREATE TABLE party_import_stage (
  id TEXT PRIMARY KEY,
  epoch TEXT NOT NULL,
  source_digest TEXT NOT NULL,
  csv_text TEXT NOT NULL,
  expires_at INTEGER NOT NULL,
  preview_id TEXT,
  preview_digest TEXT,
  mapping TEXT
);
CREATE TABLE party_import_batch (
  id TEXT PRIMARY KEY,
  source_digest TEXT NOT NULL,
  mapping TEXT NOT NULL,
  created_at TEXT NOT NULL
);
CREATE TABLE party_import_origin (
  party_id TEXT PRIMARY KEY REFERENCES entity(id) ON DELETE CASCADE,
  batch_id TEXT NOT NULL REFERENCES party_import_batch(id),
  source_record INTEGER NOT NULL,
  UNIQUE(batch_id, source_record)
);
