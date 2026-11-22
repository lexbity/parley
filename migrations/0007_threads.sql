-- S07: deadline equality and party-wide suppression (RFC-0002 BE-08).
--
-- One contact thread exists per opportunity/recipient pair; touches are
-- numbered on that thread; every send binds the exact released material
-- versions (primary plus companions) through one-use release references and
-- snapshots the gate that authorized it. Timeouts are keyed by thread,
-- current touch and expiry so restart/retry/concurrent shells produce exactly
-- one timeout event per touch. A decline is party-wide: it suppresses the
-- party across every opportunity and closes the affected opportunities only
-- when no other live contact remains.

CREATE TABLE contact_thread (
  id TEXT PRIMARY KEY,
  opportunity_id TEXT NOT NULL,
  recipient_id TEXT NOT NULL,
  state TEXT NOT NULL CHECK(state IN
    ('waiting','conversing','timed_out','closed_declined','resolved','dormant')),
  current_touch INTEGER NOT NULL DEFAULT 0 CHECK(current_touch >= 0),
  expires_at TEXT,
  sent_at TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  UNIQUE(opportunity_id, recipient_id),
  FOREIGN KEY(opportunity_id, recipient_id) REFERENCES opportunity_party(opportunity_id, party_id) ON DELETE RESTRICT
);
CREATE INDEX due_threads ON contact_thread(expires_at, id) WHERE state='waiting';

CREATE TABLE send (
  id TEXT PRIMARY KEY,
  thread_id TEXT NOT NULL REFERENCES contact_thread(id) ON DELETE RESTRICT,
  touch_no INTEGER NOT NULL CHECK(touch_no > 0),
  primary_material_id TEXT NOT NULL REFERENCES material(id) ON DELETE RESTRICT,
  primary_version_no INTEGER NOT NULL,
  recipient_id TEXT NOT NULL REFERENCES entity(id) ON DELETE RESTRICT,
  gate_snapshot_json TEXT NOT NULL,
  overrides_json TEXT NOT NULL,
  content_sha256 TEXT NOT NULL,
  sent_at TEXT NOT NULL,
  response_deadline TEXT NOT NULL,
  deadline_entered_date TEXT,
  deadline_zone TEXT NOT NULL,
  capacity_charged INTEGER NOT NULL DEFAULT 0 CHECK(capacity_charged IN (0,1)),
  UNIQUE(thread_id, touch_no)
);
CREATE INDEX sends_by_thread ON send(thread_id, touch_no);
CREATE INDEX sends_by_recipient ON send(recipient_id, sent_at);

CREATE TABLE send_part (
  send_id TEXT NOT NULL REFERENCES send(id) ON DELETE RESTRICT,
  material_id TEXT NOT NULL REFERENCES material(id) ON DELETE RESTRICT,
  version_no INTEGER NOT NULL,
  release_id TEXT NOT NULL REFERENCES release_record(id) ON DELETE RESTRICT,
  kind TEXT NOT NULL CHECK(kind IN ('primary','companion')),
  PRIMARY KEY(send_id, material_id, version_no)
);
CREATE UNIQUE INDEX release_once ON send_part(release_id);

CREATE TABLE outcome (
  id TEXT PRIMARY KEY,
  send_id TEXT NOT NULL UNIQUE REFERENCES send(id) ON DELETE RESTRICT,
  class TEXT NOT NULL CHECK(class IN
    ('no_reply','reply_positive','reply_neutral','reply_negative','declined','rung_accepted','call_scheduled')),
  rung_accepted INTEGER CHECK(rung_accepted IS NULL OR rung_accepted >= 1),
  received_at TEXT,
  recorded_at TEXT NOT NULL,
  note TEXT
);

CREATE TABLE resolution (
  id TEXT PRIMARY KEY,
  opportunity_id TEXT NOT NULL REFERENCES opportunity(id) ON DELETE RESTRICT,
  thread_id TEXT REFERENCES contact_thread(id) ON DELETE SET NULL,
  result TEXT NOT NULL CHECK(result IN ('mutual_win','one_sided','no_deal')),
  they_benefited TEXT NOT NULL CHECK(they_benefited IN ('yes','partial','no','unknown')),
  i_benefited TEXT NOT NULL CHECK(i_benefited IN ('yes','partial','no','unknown')),
  noted_at TEXT NOT NULL
);

CREATE TABLE capacity_ledger (
  period TEXT NOT NULL,
  unit TEXT NOT NULL,
  total REAL NOT NULL CHECK(total >= 0),
  committed REAL NOT NULL DEFAULT 0 CHECK(committed >= 0),
  updated_at TEXT NOT NULL,
  PRIMARY KEY(period, unit)
);
INSERT INTO capacity_ledger(period,unit,total,updated_at)
VALUES('quarter','project slots',15,strftime('%Y-%m-%dT%H:%M:%fZ','now'));
