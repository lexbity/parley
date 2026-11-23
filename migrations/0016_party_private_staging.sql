ALTER TABLE blob_upload ADD COLUMN card_private INTEGER NOT NULL DEFAULT 0 CHECK(card_private IN (0,1));
CREATE TABLE party_stage_receipt (
  stage_id TEXT NOT NULL REFERENCES party_import_stage(id) ON DELETE CASCADE,
  epoch TEXT NOT NULL,
  key TEXT NOT NULL,
  PRIMARY KEY(epoch,key)
);
CREATE TRIGGER discard_party_stage_receipts BEFORE DELETE ON party_import_stage BEGIN
  DELETE FROM idempotency_receipt WHERE owner_id=1 AND epoch=OLD.epoch AND key IN
  (SELECT key FROM party_stage_receipt WHERE stage_id=OLD.id AND epoch=OLD.epoch);
END;
