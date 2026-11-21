CREATE TABLE opportunity (
  id TEXT PRIMARY KEY,
  title TEXT NOT NULL CHECK(length(title) BETWEEN 1 AND 500),
  state TEXT NOT NULL CHECK(state IN ('open','resolved','dormant','closed')),
  close_reason TEXT,
  revision INTEGER NOT NULL CHECK(revision > 0),
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);
CREATE TABLE opportunity_party (
  opportunity_id TEXT NOT NULL REFERENCES opportunity(id) ON DELETE RESTRICT,
  party_id TEXT NOT NULL REFERENCES entity(id) ON DELETE RESTRICT,
  role TEXT NOT NULL CHECK(role IN ('primary','participant')),
  added_at TEXT NOT NULL,
  PRIMARY KEY(opportunity_id, party_id)
);
CREATE INDEX opportunity_party_by_party ON opportunity_party(party_id, opportunity_id);
CREATE TABLE party_policy (
  party_id TEXT PRIMARY KEY REFERENCES entity(id) ON DELETE RESTRICT,
  state TEXT NOT NULL DEFAULT 'open' CHECK(state IN ('open','closed','dormant')),
  do_not_contact INTEGER NOT NULL DEFAULT 0 CHECK(do_not_contact IN (0,1)),
  highest_rung INTEGER NOT NULL DEFAULT 0 CHECK(highest_rung >= 0),
  touch_count INTEGER NOT NULL DEFAULT 0 CHECK(touch_count >= 0),
  revision INTEGER NOT NULL DEFAULT 1 CHECK(revision > 0)
);
INSERT INTO party_policy(party_id) SELECT id FROM entity;
CREATE TRIGGER source_opportunity_insert_guard
BEFORE INSERT ON source_opportunity_link
WHEN NOT EXISTS (SELECT 1 FROM opportunity WHERE id=NEW.opportunity_id)
  OR NOT EXISTS (SELECT 1 FROM source_variant WHERE id=NEW.source_variant_id)
BEGIN SELECT RAISE(ABORT, 'source or opportunity does not exist'); END;
CREATE TRIGGER source_opportunity_update_guard
BEFORE UPDATE OF opportunity_id,source_variant_id ON source_opportunity_link
WHEN NOT EXISTS (SELECT 1 FROM opportunity WHERE id=NEW.opportunity_id)
  OR NOT EXISTS (SELECT 1 FROM source_variant WHERE id=NEW.source_variant_id)
BEGIN SELECT RAISE(ABORT, 'source or opportunity does not exist'); END;

CREATE TABLE opportunity_interest (
  id TEXT PRIMARY KEY,
  opportunity_id TEXT NOT NULL,
  party_id TEXT NOT NULL,
  side TEXT NOT NULL CHECK(side IN ('them','me')),
  statement TEXT NOT NULL CHECK(length(trim(statement)) > 0),
  knowledge_kind TEXT NOT NULL CHECK(knowledge_kind IN ('observation','hypothesis','owner_report')),
  provenance_kind TEXT NOT NULL CHECK(provenance_kind IN ('owner_report','source_claim')),
  claim_id TEXT,
  claim_revision INTEGER,
  revision INTEGER NOT NULL CHECK(revision > 0),
  status TEXT NOT NULL CHECK(status IN ('active','dismissed')),
  created_at TEXT NOT NULL,
  FOREIGN KEY(opportunity_id, party_id) REFERENCES opportunity_party(opportunity_id, party_id) ON DELETE RESTRICT,
  FOREIGN KEY(claim_id, claim_revision) REFERENCES claim_revision(claim_id, revision),
  CHECK((provenance_kind='source_claim' AND claim_id IS NOT NULL AND claim_revision IS NOT NULL)
     OR (provenance_kind='owner_report' AND claim_id IS NULL AND claim_revision IS NULL))
);
CREATE INDEX opportunity_interest_scope ON opportunity_interest(opportunity_id,party_id,side,status);
CREATE TRIGGER cited_interest_revision_invalidation
AFTER UPDATE OF current_revision ON claim
WHEN OLD.current_revision <> NEW.current_revision
BEGIN
  UPDATE opportunity_interest SET revision=revision+1
    WHERE claim_id=NEW.id AND claim_revision <> NEW.current_revision;
  UPDATE opportunity SET revision=revision+1,updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now')
    WHERE id IN (SELECT opportunity_id FROM opportunity_interest WHERE claim_id=NEW.id);
  INSERT INTO event(event_id,occurred_at,entity_type,entity_id,revision,event_type,payload_json)
    SELECT lower(hex(randomblob(16))),strftime('%Y-%m-%dT%H:%M:%fZ','now'),'opportunity',o.id,o.revision,
      'opportunity.source_claim_corrected',json_object('claim_id',NEW.id,'claim_revision',NEW.current_revision)
    FROM opportunity o WHERE o.id IN (SELECT opportunity_id FROM opportunity_interest WHERE claim_id=NEW.id);
END;

CREATE TABLE exchange_version (
  opportunity_id TEXT NOT NULL,
  recipient_id TEXT NOT NULL,
  version_no INTEGER NOT NULL CHECK(version_no > 0),
  they_get TEXT NOT NULL,
  i_get TEXT NOT NULL,
  fairness_attested INTEGER NOT NULL DEFAULT 0 CHECK(fairness_attested IN (0,1)),
  attestation_digest TEXT,
  attested_at TEXT,
  created_at TEXT NOT NULL,
  PRIMARY KEY(opportunity_id,recipient_id,version_no),
  FOREIGN KEY(opportunity_id,recipient_id) REFERENCES opportunity_party(opportunity_id,party_id) ON DELETE RESTRICT,
  CHECK((fairness_attested=0 AND attestation_digest IS NULL AND attested_at IS NULL)
     OR (fairness_attested=1 AND attestation_digest IS NOT NULL AND attested_at IS NOT NULL))
);
CREATE TABLE exchange_interest_link (
  opportunity_id TEXT NOT NULL,
  recipient_id TEXT NOT NULL,
  exchange_version INTEGER NOT NULL,
  interest_id TEXT NOT NULL REFERENCES opportunity_interest(id) ON DELETE RESTRICT,
  PRIMARY KEY(opportunity_id,recipient_id,exchange_version,interest_id),
  FOREIGN KEY(opportunity_id,recipient_id,exchange_version) REFERENCES exchange_version(opportunity_id,recipient_id,version_no) ON DELETE RESTRICT
);

CREATE TABLE template_family (
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  created_at TEXT NOT NULL
);
CREATE TABLE template_version (
  id TEXT PRIMARY KEY,
  family_id TEXT NOT NULL REFERENCES template_family(id) ON DELETE RESTRICT,
  version_no INTEGER NOT NULL CHECK(version_no > 0),
  body TEXT NOT NULL,
  fallbacks_json TEXT NOT NULL,
  status TEXT NOT NULL CHECK(status IN ('draft','approved','active','flagged','retired')),
  content_sha256 TEXT NOT NULL,
  created_at TEXT NOT NULL,
  UNIQUE(family_id,version_no)
);

CREATE TABLE material (
  id TEXT PRIMARY KEY,
  opportunity_id TEXT NOT NULL,
  recipient_id TEXT NOT NULL,
  kind TEXT NOT NULL CHECK(kind IN ('outreach','followup','handoff')),
  head_version INTEGER NOT NULL CHECK(head_version > 0),
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  FOREIGN KEY(opportunity_id,recipient_id) REFERENCES opportunity_party(opportunity_id,party_id) ON DELETE RESTRICT
);
CREATE INDEX material_opportunity_contact ON material(opportunity_id,recipient_id,updated_at);
CREATE TABLE material_version (
  material_id TEXT NOT NULL REFERENCES material(id) ON DELETE RESTRICT,
  version_no INTEGER NOT NULL CHECK(version_no > 0),
  opportunity_id TEXT NOT NULL,
  recipient_id TEXT NOT NULL,
  template_version_id TEXT NOT NULL REFERENCES template_version(id) ON DELETE RESTRICT,
  exchange_version INTEGER NOT NULL,
  body TEXT NOT NULL,
  slot_fills_json TEXT NOT NULL,
  sentence_bindings_json TEXT NOT NULL,
  content_sha256 TEXT NOT NULL,
  created_at TEXT NOT NULL,
  PRIMARY KEY(material_id,version_no),
  FOREIGN KEY(material_id) REFERENCES material(id) ON DELETE RESTRICT,
  FOREIGN KEY(opportunity_id,recipient_id) REFERENCES opportunity_party(opportunity_id,party_id) ON DELETE RESTRICT
);
CREATE TABLE version_dependency (
  material_id TEXT NOT NULL,
  version_no INTEGER NOT NULL,
  kind TEXT NOT NULL CHECK(kind IN ('resource','claim','exchange','template','party','opportunity')),
  source_id TEXT NOT NULL,
  source_revision TEXT NOT NULL,
  PRIMARY KEY(material_id,version_no,kind,source_id),
  FOREIGN KEY(material_id,version_no) REFERENCES material_version(material_id,version_no) ON DELETE RESTRICT
);
CREATE INDEX affected_versions ON version_dependency(kind,source_id,source_revision,material_id,version_no);
CREATE TABLE approval (
  id TEXT PRIMARY KEY,
  material_id TEXT NOT NULL,
  version_no INTEGER NOT NULL,
  content_sha256 TEXT NOT NULL,
  dependency_vector_json TEXT NOT NULL,
  gate_snapshot_json TEXT NOT NULL,
  overrides_json TEXT NOT NULL,
  approved_at TEXT NOT NULL,
  UNIQUE(material_id,version_no),
  FOREIGN KEY(material_id,version_no) REFERENCES material_version(material_id,version_no) ON DELETE RESTRICT
);
CREATE TABLE release_record (
  id TEXT PRIMARY KEY,
  approval_id TEXT NOT NULL REFERENCES approval(id) ON DELETE RESTRICT,
  material_id TEXT NOT NULL,
  version_no INTEGER NOT NULL,
  content_sha256 TEXT NOT NULL,
  issued_at TEXT NOT NULL,
  acknowledged INTEGER CHECK(acknowledged IN (0,1)),
  acknowledged_at TEXT,
  FOREIGN KEY(material_id,version_no) REFERENCES material_version(material_id,version_no) ON DELETE RESTRICT
);
CREATE INDEX releases_by_material ON release_record(material_id,version_no,issued_at);
CREATE TABLE release_ack (
  release_id TEXT PRIMARY KEY REFERENCES release_record(id) ON DELETE RESTRICT,
  succeeded INTEGER NOT NULL CHECK(succeeded IN (0,1)),
  acknowledged_at TEXT NOT NULL
);

CREATE TABLE resource_use (
  material_id TEXT NOT NULL,
  version_no INTEGER NOT NULL,
  resource_id TEXT NOT NULL REFERENCES resource(id) ON DELETE RESTRICT,
  slot_name TEXT NOT NULL,
  PRIMARY KEY(material_id,version_no,resource_id,slot_name),
  FOREIGN KEY(material_id,version_no) REFERENCES material_version(material_id,version_no) ON DELETE RESTRICT
);

CREATE TRIGGER material_version_no_update BEFORE UPDATE ON material_version
BEGIN SELECT RAISE(ABORT, 'material versions are immutable'); END;
CREATE TRIGGER material_version_no_delete BEFORE DELETE ON material_version
BEGIN SELECT RAISE(ABORT, 'material versions are immutable'); END;
CREATE TRIGGER template_content_immutable BEFORE UPDATE OF family_id,version_no,body,fallbacks_json,content_sha256,created_at ON template_version
BEGIN SELECT RAISE(ABORT, 'template version content is immutable'); END;
CREATE TRIGGER template_version_no_delete BEFORE DELETE ON template_version
BEGIN SELECT RAISE(ABORT, 'template versions are immutable'); END;
CREATE TRIGGER approval_no_update BEFORE UPDATE ON approval
BEGIN SELECT RAISE(ABORT, 'approval records are immutable'); END;
CREATE TRIGGER approval_no_delete BEFORE DELETE ON approval
BEGIN SELECT RAISE(ABORT, 'approval records are immutable'); END;
CREATE TRIGGER release_no_update BEFORE UPDATE ON release_record
BEGIN SELECT RAISE(ABORT, 'release records are immutable'); END;
CREATE TRIGGER release_no_delete BEFORE DELETE ON release_record
BEGIN SELECT RAISE(ABORT, 'release records are immutable'); END;
