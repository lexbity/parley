-- S06: verified file issuance across power loss (RFC-0002 BQ4/BE-11).
--
-- Content-addressed attachments are immutable; the database owns reachability.
-- A committed upload lease protects the window between atomic installation and
-- the blob/upload commit. Garbage collection rechecks leases, snapshot pins
-- and live references under the maintenance lock immediately before unlink.

CREATE TABLE blob (
  sha256 TEXT PRIMARY KEY CHECK(length(sha256) = 64),
  size INTEGER NOT NULL CHECK(size > 0),
  mime TEXT NOT NULL CHECK(length(mime) BETWEEN 1 AND 200),
  installed_at TEXT NOT NULL
);

CREATE TABLE blob_upload (
  id TEXT PRIMARY KEY,
  blob_sha256 TEXT NOT NULL REFERENCES blob(sha256) ON DELETE RESTRICT,
  filename TEXT NOT NULL CHECK(length(filename) BETWEEN 1 AND 500),
  mime TEXT NOT NULL CHECK(length(mime) BETWEEN 1 AND 200),
  size INTEGER NOT NULL CHECK(size > 0),
  share_permission INTEGER NOT NULL DEFAULT 0 CHECK(share_permission IN (0,1)),
  reviewed_by_owner INTEGER NOT NULL DEFAULT 0 CHECK(reviewed_by_owner IN (0,1)),
  revision INTEGER NOT NULL CHECK(revision > 0),
  created_at TEXT NOT NULL
);
CREATE INDEX blob_upload_by_digest ON blob_upload(blob_sha256, created_at);

-- Committed before a newly installed file is published, deleted in the same
-- transaction that publishes the blob/upload rows. GC never unlinks a file
-- whose digest still has an unexpired lease.
CREATE TABLE blob_upload_lease (
  upload_id TEXT PRIMARY KEY,
  blob_sha256 TEXT NOT NULL CHECK(length(blob_sha256) = 64),
  size INTEGER NOT NULL CHECK(size > 0),
  created_at TEXT NOT NULL
);

-- Durable pins held by online snapshots/backups. A pin may outlive the blob
-- row it protects (erasure or GC can remove subject rows while a snapshot
-- still pins the bytes); GC therefore treats pins as first-class references
-- and never unlinks a pinned digest.
CREATE TABLE blob_snapshot_pin (
  snapshot_id TEXT NOT NULL,
  blob_sha256 TEXT NOT NULL CHECK(length(blob_sha256) = 64),
  created_at TEXT NOT NULL,
  PRIMARY KEY(snapshot_id, blob_sha256)
);
CREATE INDEX blob_pin_by_digest ON blob_snapshot_pin(blob_sha256, snapshot_id);

-- A material version may carry one attachment. The version binds a specific
-- upload record (distinct owner review permission per upload) while byte
-- identity is shared through the blob digest.
ALTER TABLE material_version ADD COLUMN blob_upload_id TEXT REFERENCES blob_upload(id) ON DELETE RESTRICT;
ALTER TABLE material_version ADD COLUMN blob_sha256 TEXT REFERENCES blob(sha256) ON DELETE RESTRICT;
ALTER TABLE material_version ADD COLUMN filename TEXT;
ALTER TABLE material_version ADD COLUMN content_type TEXT;
ALTER TABLE material_version ADD COLUMN attachment_size INTEGER;

CREATE INDEX material_blob_upload_lookup ON material_version(blob_upload_id);

-- Extend the dependency-kind registry with the blob reference kind so an
-- attachment version carries its upload revision in the same dependency
-- vector that revokes stale approvals and releases.
DROP TABLE version_dependency;
CREATE TABLE version_dependency (
  material_id TEXT NOT NULL,
  version_no INTEGER NOT NULL,
  kind TEXT NOT NULL CHECK(kind IN ('resource','claim','exchange','template','party','opportunity','blob')),
  source_id TEXT NOT NULL,
  source_revision TEXT NOT NULL,
  PRIMARY KEY(material_id,version_no,kind,source_id),
  FOREIGN KEY(material_id,version_no) REFERENCES material_version(material_id,version_no) ON DELETE RESTRICT
);
CREATE INDEX affected_versions ON version_dependency(kind,source_id,source_revision,material_id,version_no);

-- Release records stay immutable except the server-side issuance fields: the
-- moment bytes begin flowing (one-use grant) and an observable incomplete
-- transfer. Everything else remains frozen.
DROP TRIGGER release_no_update;
CREATE TRIGGER release_issuance_only BEFORE UPDATE ON release_record
WHEN NOT (NEW.id = OLD.id AND NEW.approval_id = OLD.approval_id
      AND NEW.material_id = OLD.material_id AND NEW.version_no = OLD.version_no
      AND NEW.content_sha256 = OLD.content_sha256 AND NEW.issued_at = OLD.issued_at
      AND NEW.acknowledged = OLD.acknowledged AND NEW.acknowledged_at IS OLD.acknowledged_at
      AND (NEW.bytes_issued_at IS OLD.bytes_issued_at
           OR (OLD.bytes_issued_at IS NULL AND NEW.bytes_issued_at IS NOT NULL))
      AND NEW.transfer_incomplete = OLD.transfer_incomplete)
BEGIN SELECT RAISE(ABORT, 'release issuance fields are append-only'); END;

ALTER TABLE release_record ADD COLUMN bytes_issued_at TEXT;
ALTER TABLE release_record ADD COLUMN transfer_incomplete INTEGER NOT NULL DEFAULT 0 CHECK(transfer_incomplete IN (0,1));
