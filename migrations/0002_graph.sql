CREATE TABLE source_content (
  id TEXT PRIMARY KEY,
  normalized_sha256 TEXT NOT NULL,
  normalization_version INTEGER NOT NULL,
  normalized_text TEXT NOT NULL,
  created_at TEXT NOT NULL
);
CREATE INDEX source_content_hash ON source_content(normalization_version, normalized_sha256);
CREATE TABLE source_variant (
  id TEXT PRIMARY KEY,
  content_id TEXT NOT NULL REFERENCES source_content(id) ON DELETE RESTRICT,
  raw_sha256 TEXT NOT NULL,
  raw_text TEXT NOT NULL,
  title TEXT,
  url TEXT,
  captured_at TEXT NOT NULL
);
CREATE INDEX source_variant_digest ON source_variant(raw_sha256);
CREATE TABLE entity (
  id TEXT PRIMARY KEY,
  kind TEXT NOT NULL CHECK (kind IN ('person','org','project','resource')),
  name TEXT NOT NULL,
  revision INTEGER NOT NULL CHECK (revision > 0),
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);
CREATE INDEX entity_name ON entity(name COLLATE NOCASE, id);
CREATE TABLE source_occurrence (
  id TEXT PRIMARY KEY,
  source_variant_id TEXT NOT NULL REFERENCES source_variant(id) ON DELETE RESTRICT,
  actor_kind TEXT NOT NULL CHECK(actor_kind IN ('owner','party','unknown')),
  actor_entity_id TEXT REFERENCES entity(id) ON DELETE RESTRICT,
  captured_at TEXT NOT NULL,
  UNIQUE(id, source_variant_id)
);
CREATE TABLE claim (
  id TEXT PRIMARY KEY,
  subject_id TEXT NOT NULL REFERENCES entity(id) ON DELETE RESTRICT,
  predicate TEXT NOT NULL,
  current_revision INTEGER NOT NULL CHECK (current_revision > 0),
  created_at TEXT NOT NULL
);
CREATE TABLE claim_revision (
  claim_id TEXT NOT NULL REFERENCES claim(id) ON DELETE RESTRICT,
  revision INTEGER NOT NULL CHECK (revision > 0),
  wording TEXT NOT NULL,
  knowledge_kind TEXT NOT NULL CHECK (knowledge_kind IN ('observation','hypothesis','owner_report')),
  tier INTEGER NOT NULL CHECK (tier IN (1,2,3)),
  speaker_kind TEXT NOT NULL CHECK (speaker_kind IN ('owner','party','unknown')),
  speaker_entity_id TEXT REFERENCES entity(id) ON DELETE RESTRICT,
  occurrence_id TEXT,
  source_variant_id TEXT,
  source_start INTEGER,
  source_end INTEGER,
  normalized_start INTEGER,
  normalized_end INTEGER,
  normalization_version INTEGER,
  approved_wording TEXT,
  check_suggestion TEXT,
  confirmed_by_owner INTEGER NOT NULL DEFAULT 0 CHECK (confirmed_by_owner IN (0,1)),
  created_at TEXT NOT NULL,
  PRIMARY KEY (claim_id, revision),
  FOREIGN KEY (occurrence_id, source_variant_id) REFERENCES source_occurrence(id, source_variant_id),
  CHECK (tier <> 1 OR (occurrence_id IS NOT NULL AND source_start IS NOT NULL AND source_end > source_start)),
  CHECK (tier <> 2 OR check_suggestion IS NOT NULL),
  CHECK (tier <> 3 OR confirmed_by_owner = 1)
);
CREATE TABLE edge (
  id TEXT PRIMARY KEY,
  from_id TEXT NOT NULL REFERENCES entity(id) ON DELETE RESTRICT,
  verb TEXT NOT NULL,
  to_id TEXT NOT NULL REFERENCES entity(id) ON DELETE RESTRICT,
  state TEXT NOT NULL CHECK (state IN ('active','retracted')),
  revision INTEGER NOT NULL CHECK (revision > 0),
  created_at TEXT NOT NULL,
  CHECK (from_id <> to_id)
);
CREATE UNIQUE INDEX active_edge ON edge(from_id, verb, to_id) WHERE state='active';
CREATE TABLE edge_evidence (
  edge_id TEXT NOT NULL REFERENCES edge(id) ON DELETE RESTRICT,
  claim_id TEXT NOT NULL REFERENCES claim(id) ON DELETE RESTRICT,
  claim_revision INTEGER NOT NULL,
  PRIMARY KEY (edge_id, claim_id, claim_revision),
  FOREIGN KEY (claim_id, claim_revision) REFERENCES claim_revision(claim_id, revision)
);
CREATE TABLE view_position (
  view_scope TEXT NOT NULL,
  view_id TEXT NOT NULL,
  entity_id TEXT NOT NULL REFERENCES entity(id) ON DELETE CASCADE,
  x REAL NOT NULL,
  y REAL NOT NULL,
  pinned INTEGER NOT NULL DEFAULT 0 CHECK (pinned IN (0,1)),
  revision INTEGER NOT NULL CHECK (revision > 0),
  PRIMARY KEY (view_scope, view_id, entity_id),
  CHECK (x = x AND y = y AND abs(x) < 1.0e308 AND abs(y) < 1.0e308)
);
CREATE TABLE proposal_batch (
  id TEXT PRIMARY KEY,
  source_variant_id TEXT NOT NULL REFERENCES source_variant(id) ON DELETE RESTRICT,
  job_id TEXT,
  status TEXT NOT NULL CHECK (status IN ('pending','partially_applied','applied','dismissed')),
  created_at TEXT NOT NULL
);
CREATE TABLE proposal_item (
  id TEXT PRIMARY KEY,
  batch_id TEXT NOT NULL REFERENCES proposal_batch(id) ON DELETE CASCADE,
  parent_id TEXT REFERENCES proposal_item(id) ON DELETE RESTRICT,
  proposal_kind TEXT NOT NULL,
  candidate_json TEXT NOT NULL,
  occurrence_id TEXT REFERENCES source_occurrence(id) ON DELETE RESTRICT,
  expected_target_revision INTEGER,
  status TEXT NOT NULL CHECK (status IN ('pending','accepted','corrected','dismissed','source_only')),
  UNIQUE(batch_id, id)
);
CREATE TABLE source_opportunity_link (
  source_variant_id TEXT NOT NULL REFERENCES source_variant(id) ON DELETE RESTRICT,
  opportunity_id TEXT NOT NULL,
  created_at TEXT NOT NULL,
  PRIMARY KEY(source_variant_id, opportunity_id)
);
