-- Owner-submitted score labels are not evaluation evidence and must never
-- drive deterministic matching. Only the offline E-match evaluator may set
-- trusted after running the versioned fixture against the active embed model.
ALTER TABLE matching_calibration
ADD COLUMN trusted INTEGER NOT NULL DEFAULT 0 CHECK(trusted IN (0,1));
