-- UCR-001 U08: command approvals for destructive registry commands.
--
-- Every destructive registry execution, for both actors, needs human
-- approval (Q5). One approval row binds the exact intent: principal, run
-- if present, epoch, command/version, idempotency key digest, wire and
-- canonical args digests, the dependency fingerprint computed by the
-- read-only planner, and an expiry. Execution recomputes the fingerprint
-- under reservation before consuming the grant with the effect; a changed
-- dependency turns the grant stale instead of silently broadening it.
--
-- At most one nonterminal approval may exist per (epoch, principal,
-- command, key digest); a wrong, denied, expired or revoked grant fails
-- closed and requires a fresh key. Pending/granted rows carry a TTL
-- (10 min default, 30 min deployment maximum) and are purged of their
-- private args/preview payloads at any terminal transition. Minimal status
-- rows are retained until receipt expiry or 24 h, whichever expires first.
--
-- Agent tool intents additionally persist their wire args so a run parked
-- on `waiting_owner` can re-dispatch the exact durable invocation binding
-- after the owner grants (Q7 / U08). The ledger meta gains a payload quota
-- counter for host/approval bytes (NFR-6).

CREATE TABLE command_approval (
  id TEXT PRIMARY KEY,
  epoch TEXT NOT NULL,
  owner_pseudonym TEXT NOT NULL,
  principal_pseudonym TEXT NOT NULL,
  run_pseudonym TEXT,
  command_id TEXT NOT NULL,
  command_version INTEGER NOT NULL,
  key_digest TEXT NOT NULL,
  wire_args_digest TEXT NOT NULL,
  canonical_args_digest TEXT NOT NULL,
  dependency_fingerprint TEXT NOT NULL,
  preview_digest TEXT NOT NULL,
  status TEXT NOT NULL CHECK(status IN ('pending','granted','consumed','denied','revoked','expired','stale')),
  revision INTEGER NOT NULL DEFAULT 1,
  args_json TEXT,
  preview_json TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  expires_at TEXT NOT NULL,
  consumed_audit_id TEXT,
  decision_key_digest TEXT,
  decision_input_digest TEXT,
  decision_outcome_json TEXT
);

CREATE UNIQUE INDEX command_approval_nonterminal_key
  ON command_approval(epoch, principal_pseudonym, command_id, key_digest)
  WHERE status IN ('pending','granted');

CREATE INDEX command_approval_owner_status
  ON command_approval(owner_pseudonym, epoch, status, expires_at);

CREATE INDEX command_approval_expiry ON command_approval(expires_at);

ALTER TABLE command_ledger_meta ADD COLUMN approval_bytes INTEGER NOT NULL DEFAULT 0;

ALTER TABLE agent_tool_intent ADD COLUMN args_json TEXT NOT NULL DEFAULT '{}';
