-- UCR-001 U01: command registry ledger.
--
-- Every adopted registry invocation is recorded as a durable attempt with
-- exactly one normalized terminal outcome (enforced below). Replays return
-- stored receipts; receipts expire 24 h after commit. The ledger is
-- append-only at the application layer; no UPDATE/DELETE triggers are
-- created so the authorized maintenance scrub (backup staging) and erasure
-- can rewrite detached copies. A private generation key held outside the
-- database produces the domain-separated HMAC key digests.

CREATE TABLE command_audit_event (
  audit_id TEXT PRIMARY KEY,
  kind TEXT NOT NULL CHECK(kind IN ('attempt','outcome')),
  epoch TEXT NOT NULL,
  actor_kind TEXT NOT NULL CHECK(actor_kind IN ('owner')),
  principal_pseudonym TEXT NOT NULL,
  command_id TEXT,
  command_version INTEGER,
  mode TEXT NOT NULL CHECK(mode IN ('execute','dry_run')),
  policy_result TEXT NOT NULL,
  outcome_code TEXT,
  attempt_audit_id TEXT,
  replay_pointer TEXT,
  key_digest TEXT,
  args_json TEXT NOT NULL DEFAULT '{}',
  error_code TEXT,
  error_message TEXT,
  request_bytes INTEGER NOT NULL DEFAULT 0,
  result_bytes INTEGER,
  duration_ms INTEGER,
  occurred_at TEXT NOT NULL
);
CREATE INDEX command_audit_attempt ON command_audit_event(epoch, actor_kind, occurred_at);
CREATE UNIQUE INDEX command_audit_one_outcome
  ON command_audit_event(attempt_audit_id)
  WHERE kind = 'outcome' AND attempt_audit_id IS NOT NULL;

CREATE TABLE command_receipt (
  epoch TEXT NOT NULL,
  principal_pseudonym TEXT NOT NULL,
  command_id TEXT NOT NULL,
  key_digest TEXT NOT NULL,
  command_version INTEGER NOT NULL,
  mode TEXT NOT NULL CHECK(mode IN ('execute','dry_run')),
  wire_args_digest TEXT NOT NULL,
  original_audit_id TEXT NOT NULL,
  response_variant TEXT NOT NULL,
  response_status INTEGER NOT NULL,
  response_bytes TEXT NOT NULL,
  committed_at TEXT NOT NULL,
  expires_at TEXT NOT NULL,
  PRIMARY KEY (epoch, principal_pseudonym, command_id, key_digest)
);
CREATE INDEX command_receipt_expiry ON command_receipt(expires_at);

-- Logical quota counters for NFR-6 fail-closed accounting.
CREATE TABLE command_ledger_meta (
  singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
  audit_bytes INTEGER NOT NULL DEFAULT 0,
  receipt_bytes INTEGER NOT NULL DEFAULT 0,
  updated_at TEXT NOT NULL
);
INSERT INTO command_ledger_meta(singleton, updated_at)
VALUES(1, '1970-01-01T00:00:00Z');
