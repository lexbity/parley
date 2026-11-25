-- Pack settings surface (GET /settings, packs/validate, packs/activate).
--
-- owner_profile gains a settings revision: the concurrency token the Packs
-- surface fences activation on (412 SETTINGS_STALE), separate from the
-- workspace revision. pack_document stores validated pack documents by
-- content digest, so activation can only ever install the exact document the
-- owner validated. Reference packs (job_search, lead_gen) are embedded in the
-- service and are not rows here until an owner activates a modified version.

ALTER TABLE owner_profile ADD COLUMN settings_revision INTEGER NOT NULL DEFAULT 1;

CREATE TABLE pack_document (
  digest TEXT PRIMARY KEY,
  pack_id TEXT NOT NULL,
  version TEXT NOT NULL,
  name TEXT NOT NULL,
  document TEXT NOT NULL,
  validated_at TEXT NOT NULL
);
CREATE INDEX pack_document_pack ON pack_document(pack_id, version);
