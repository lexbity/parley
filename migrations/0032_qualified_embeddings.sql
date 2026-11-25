-- AR-001 AR07. Qualified embeddings: one pinned worker transport, recipe-bound
-- cache and calibration, readable historical calibration rows, and per-mode
-- probes. Forward-only under the workspace lock.
PRAGMA defer_foreign_keys = ON;

-- Probes declare which capability they exercise; readiness is per mode.
ALTER TABLE inference_probe ADD COLUMN mode TEXT NOT NULL DEFAULT 'structured_role'
CHECK (mode IN ('structured_role','embed'));

-- Qualifications record the observed model identity (embed digest) and the
-- probe-observed vector width for the embed mode.
ALTER TABLE inference_qualification ADD COLUMN model_digest TEXT;
ALTER TABLE inference_qualification ADD COLUMN details_json TEXT NOT NULL DEFAULT '{}'
CHECK (json_valid(details_json) AND json_type(details_json) = 'object');

-- Embedding vectors are transient. Rebuild the cache under a recipe digest so
-- a changed model, dimension or scoring configuration can never reuse old
-- vectors. Legacy rows are disposable and dropped with the old key shape.
DROP TABLE embedding_cache;
CREATE TABLE embedding_cache (
  recipe_digest TEXT NOT NULL,
  model_digest TEXT NOT NULL,
  text_sha256 TEXT NOT NULL,
  dimensions INTEGER NOT NULL CHECK(dimensions > 0),
  vector_json TEXT NOT NULL,
  created_at TEXT NOT NULL,
  PRIMARY KEY(recipe_digest,text_sha256)
);

-- Calibration rows gain the recipe binding and held-out evaluation metrics.
-- Historical trusted rows keep their previous keys and remain readable; they
-- can never satisfy a real recipe query.
ALTER TABLE matching_calibration ADD COLUMN recipe_digest TEXT NOT NULL DEFAULT '';
ALTER TABLE matching_calibration ADD COLUMN accuracy REAL
CHECK (accuracy IS NULL OR (accuracy >= 0.0 AND accuracy <= 1.0));
ALTER TABLE matching_calibration ADD COLUMN precision REAL
CHECK (precision IS NULL OR (precision >= 0.0 AND precision <= 1.0));
ALTER TABLE matching_calibration ADD COLUMN recall REAL
CHECK (recall IS NULL OR (recall >= 0.0 AND recall <= 1.0));
CREATE INDEX qualified_match_calibration
ON matching_calibration(recipe_digest,model_digest,asset_kind,calibration_revision DESC)
WHERE trusted = 1;
CREATE INDEX match_calibration_recipe_kind
ON matching_calibration(recipe_digest,asset_kind,calibration_revision DESC);

-- There runs carry the recipe used to produce them so a later model change is
-- visible as a stale assessment rather than a false current one.
ALTER TABLE match_run ADD COLUMN recipe_digest TEXT;
CREATE INDEX match_run_recipe ON match_run(recipe_digest,created_at DESC,id)
WHERE recipe_digest IS NOT NULL;

CREATE TEMP TABLE ar07_preflight (check_id INTEGER);
CREATE TEMP TRIGGER ar07_reject_orphans BEFORE INSERT ON ar07_preflight
WHEN EXISTS (SELECT 1 FROM pragma_foreign_key_check)
BEGIN SELECT RAISE(ABORT, 'INFERENCE_EMBED_LINEAGE_ORPHAN'); END;
INSERT INTO ar07_preflight VALUES (1);
DROP TABLE ar07_preflight;
