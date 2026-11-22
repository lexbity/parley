CREATE TABLE opportunity (
  id TEXT PRIMARY KEY,
  title TEXT NOT NULL CHECK(length(trim(title)) BETWEEN 1 AND 240),
  status TEXT NOT NULL CHECK(status IN ('open','resolved','timed_out','archived')) DEFAULT 'open',
  revision INTEGER NOT NULL DEFAULT 1 CHECK(revision > 0),
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);

CREATE TABLE opportunity_participant (
  opportunity_id TEXT NOT NULL REFERENCES opportunity(id) ON DELETE CASCADE,
  entity_id TEXT NOT NULL REFERENCES entity(id) ON DELETE RESTRICT,
  participant_role TEXT NOT NULL CHECK(participant_role IN ('primary','participant')) DEFAULT 'participant',
  created_at TEXT NOT NULL,
  PRIMARY KEY(opportunity_id, entity_id)
);
CREATE INDEX opportunity_participant_entity ON opportunity_participant(entity_id, opportunity_id);

CREATE TRIGGER opportunity_participant_kind
BEFORE INSERT ON opportunity_participant
WHEN (SELECT kind FROM entity WHERE id=NEW.entity_id) NOT IN ('person','org')
BEGIN SELECT RAISE(ABORT, 'opportunity participants must be people or organizations'); END;

CREATE TABLE opportunity_interest (
  id TEXT PRIMARY KEY,
  opportunity_id TEXT NOT NULL REFERENCES opportunity(id) ON DELETE CASCADE,
  side TEXT NOT NULL CHECK(side IN ('them','me')),
  statement TEXT NOT NULL CHECK(length(trim(statement)) BETWEEN 1 AND 2000),
  provenance_tier INTEGER NOT NULL CHECK(provenance_tier IN (1,2,3)),
  source_claim_id TEXT,
  source_claim_revision INTEGER,
  status TEXT NOT NULL CHECK(status IN ('active','dismissed')) DEFAULT 'active',
  created_at TEXT NOT NULL,
  CHECK((provenance_tier=1 AND source_claim_id IS NOT NULL AND source_claim_revision IS NOT NULL) OR
        (provenance_tier IN (2,3) AND ((source_claim_id IS NULL AND source_claim_revision IS NULL) OR (source_claim_id IS NOT NULL AND source_claim_revision IS NOT NULL)))),
  FOREIGN KEY(source_claim_id,source_claim_revision) REFERENCES claim_revision(claim_id,revision)
);
CREATE INDEX opportunity_interest_active ON opportunity_interest(opportunity_id,side,status);

CREATE TABLE opportunity_exchange (
  id TEXT PRIMARY KEY,
  opportunity_id TEXT NOT NULL REFERENCES opportunity(id) ON DELETE CASCADE,
  version INTEGER NOT NULL CHECK(version > 0),
  they_get TEXT NOT NULL CHECK(length(trim(they_get)) BETWEEN 1 AND 4000),
  i_get TEXT NOT NULL CHECK(length(trim(i_get)) BETWEEN 1 AND 4000),
  fairness_attested INTEGER NOT NULL DEFAULT 0 CHECK(fairness_attested IN (0,1)),
  attested_at TEXT,
  created_at TEXT NOT NULL,
  UNIQUE(opportunity_id,version),
  CHECK((fairness_attested=1 AND attested_at IS NOT NULL) OR fairness_attested=0)
);
CREATE TABLE opportunity_exchange_interest (
  exchange_id TEXT NOT NULL REFERENCES opportunity_exchange(id) ON DELETE CASCADE,
  interest_id TEXT NOT NULL REFERENCES opportunity_interest(id) ON DELETE RESTRICT,
  PRIMARY KEY(exchange_id,interest_id)
);
CREATE TRIGGER exchange_interest_same_opportunity
BEFORE INSERT ON opportunity_exchange_interest
WHEN (SELECT opportunity_id FROM opportunity_exchange WHERE id=NEW.exchange_id) !=
     (SELECT opportunity_id FROM opportunity_interest WHERE id=NEW.interest_id)
BEGIN SELECT RAISE(ABORT, 'exchange interest must belong to the same opportunity'); END;
CREATE TRIGGER exchange_immutable_update
BEFORE UPDATE ON opportunity_exchange
BEGIN SELECT RAISE(ABORT, 'exchange versions are immutable'); END;
CREATE TRIGGER exchange_immutable_delete
BEFORE DELETE ON opportunity_exchange
BEGIN SELECT RAISE(ABORT, 'exchange versions are immutable'); END;

CREATE TABLE material (
  id TEXT PRIMARY KEY,
  opportunity_id TEXT NOT NULL REFERENCES opportunity(id) ON DELETE RESTRICT,
  recipient_id TEXT NOT NULL REFERENCES entity(id) ON DELETE RESTRICT,
  kind TEXT NOT NULL CHECK(kind IN ('outreach','follow_up','handoff')),
  title TEXT NOT NULL CHECK(length(trim(title)) BETWEEN 1 AND 240),
  current_version INTEGER NOT NULL DEFAULT 1 CHECK(current_version > 0),
  revision INTEGER NOT NULL DEFAULT 1 CHECK(revision > 0),
  status TEXT NOT NULL CHECK(status IN ('draft','approved','retired')) DEFAULT 'draft',
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  UNIQUE(id,opportunity_id,recipient_id)
);
CREATE INDEX material_opportunity ON material(opportunity_id,status,updated_at DESC,id);
CREATE INDEX material_recipient ON material(recipient_id,opportunity_id,status);
CREATE TRIGGER material_recipient_kind
BEFORE INSERT ON material
WHEN (SELECT kind FROM entity WHERE id=NEW.recipient_id) NOT IN ('person','org')
BEGIN SELECT RAISE(ABORT, 'material recipient must be a person or organization'); END;

CREATE TABLE material_version (
  material_id TEXT NOT NULL REFERENCES material(id) ON DELETE RESTRICT,
  version INTEGER NOT NULL CHECK(version > 0),
  body TEXT NOT NULL CHECK(length(body) BETWEEN 0 AND 20000),
  sentences_json TEXT NOT NULL CHECK(json_valid(sentences_json)),
  slot_fills_json TEXT NOT NULL CHECK(json_valid(slot_fills_json)),
  protected_tokens_json TEXT NOT NULL CHECK(json_valid(protected_tokens_json)),
  rules_json TEXT NOT NULL CHECK(json_valid(rules_json)),
  content_sha256 TEXT NOT NULL CHECK(length(content_sha256)=64),
  created_at TEXT NOT NULL,
  PRIMARY KEY(material_id,version)
);
CREATE TRIGGER material_version_immutable_update
BEFORE UPDATE ON material_version
BEGIN SELECT RAISE(ABORT, 'material versions are immutable'); END;
CREATE TRIGGER material_version_immutable_delete
BEFORE DELETE ON material_version
BEGIN SELECT RAISE(ABORT, 'material versions are immutable'); END;

CREATE TABLE material_approval (
  material_id TEXT NOT NULL REFERENCES material(id) ON DELETE RESTRICT,
  version INTEGER NOT NULL,
  recipient_id TEXT NOT NULL REFERENCES entity(id) ON DELETE RESTRICT,
  gate_snapshot_json TEXT NOT NULL CHECK(json_valid(gate_snapshot_json)),
  override_reasons_json TEXT NOT NULL CHECK(json_valid(override_reasons_json)),
  body_sha256 TEXT NOT NULL CHECK(length(body_sha256)=64),
  approved_at TEXT NOT NULL,
  PRIMARY KEY(material_id,version),
  FOREIGN KEY(material_id,version) REFERENCES material_version(material_id,version)
);
CREATE TRIGGER material_approval_immutable_update
BEFORE UPDATE ON material_approval
BEGIN SELECT RAISE(ABORT, 'approval records are immutable'); END;
CREATE TRIGGER material_approval_immutable_delete
BEFORE DELETE ON material_approval
BEGIN SELECT RAISE(ABORT, 'approval records are immutable'); END;

CREATE TABLE copy_grant (
  id TEXT PRIMARY KEY,
  owner_id INTEGER NOT NULL DEFAULT 1 REFERENCES owner_profile(id),
  epoch TEXT NOT NULL,
  session_id TEXT NOT NULL,
  material_id TEXT NOT NULL,
  opportunity_id TEXT NOT NULL,
  version INTEGER NOT NULL,
  recipient_id TEXT NOT NULL,
  content_sha256 TEXT NOT NULL CHECK(length(content_sha256)=64),
  content TEXT NOT NULL,
  issued_at TEXT NOT NULL,
  expires_at TEXT NOT NULL,
  acknowledged_at TEXT,
  FOREIGN KEY(material_id,version) REFERENCES material_version(material_id,version),
  FOREIGN KEY(material_id,opportunity_id,recipient_id) REFERENCES material(id,opportunity_id,recipient_id)
);
CREATE INDEX copy_grant_expiry ON copy_grant(expires_at);

CREATE TABLE copy_acknowledgement (
  owner_id INTEGER NOT NULL DEFAULT 1 REFERENCES owner_profile(id),
  epoch TEXT NOT NULL,
  operation_key TEXT NOT NULL,
  canonical_digest TEXT NOT NULL,
  grant_id TEXT NOT NULL REFERENCES copy_grant(id) ON DELETE RESTRICT,
  success INTEGER NOT NULL CHECK(success IN (0,1)),
  acknowledged_at TEXT NOT NULL,
  PRIMARY KEY(owner_id,epoch,operation_key)
);
CREATE INDEX copy_ack_grant ON copy_acknowledgement(grant_id,acknowledged_at);
