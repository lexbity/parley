-- UCR-001 U02: in-process delegated read runs.
--
-- A run is an immutable owner grant plus a mutable, fenced state machine. The
-- grant (permissions and target records) is fixed at start; a cancellation
-- revokes it at the writer boundary. Tool intents are persisted before a
-- provider result is dispatched so a restart can reconcile the same ordinal
-- and key instead of inventing a second invocation.

-- U01 constrained audit actors to the owner. A delegated run creates a
-- distinct, pseudonymous agent principal; rebuild the table so both kinds
-- can be recorded without weakening the one-outcome-per-attempt invariant.
CREATE TABLE command_audit_event_next (
  audit_id TEXT PRIMARY KEY,
  kind TEXT NOT NULL CHECK(kind IN ('attempt','outcome')),
  epoch TEXT NOT NULL,
  actor_kind TEXT NOT NULL CHECK(actor_kind IN ('owner','agent')),
  principal_pseudonym TEXT NOT NULL,
  run_pseudonym TEXT,
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
INSERT INTO command_audit_event_next
  (audit_id,kind,epoch,actor_kind,principal_pseudonym,run_pseudonym,command_id,command_version,mode,
   policy_result,outcome_code,attempt_audit_id,replay_pointer,key_digest,args_json,error_code,
   error_message,request_bytes,result_bytes,duration_ms,occurred_at)
SELECT audit_id,kind,epoch,actor_kind,principal_pseudonym,NULL,command_id,command_version,mode,
   policy_result,outcome_code,attempt_audit_id,replay_pointer,key_digest,args_json,error_code,
   error_message,request_bytes,result_bytes,duration_ms,occurred_at
FROM command_audit_event;
DROP TABLE command_audit_event;
ALTER TABLE command_audit_event_next RENAME TO command_audit_event;
CREATE INDEX command_audit_attempt ON command_audit_event(epoch, actor_kind, occurred_at);
CREATE UNIQUE INDEX command_audit_one_outcome
  ON command_audit_event(attempt_audit_id)
  WHERE kind = 'outcome' AND attempt_audit_id IS NOT NULL;

CREATE TABLE agent_run (
  run_id TEXT PRIMARY KEY,
  epoch TEXT NOT NULL,
  owner_pseudonym TEXT NOT NULL,
  task TEXT NOT NULL,
  key_digest TEXT NOT NULL,
  state TEXT NOT NULL CHECK(state IN ('queued','running','waiting_owner','waiting_job','succeeded','failed','cancelled','expired')),
  state_revision INTEGER NOT NULL DEFAULT 1,
  grant_json TEXT NOT NULL,
  scope_digest TEXT NOT NULL,
  lease_token TEXT,
  lease_deadline TEXT,
  wake_condition TEXT,
  turn_count INTEGER NOT NULL DEFAULT 0,
  tool_count INTEGER NOT NULL DEFAULT 0,
  transcript_bytes INTEGER NOT NULL DEFAULT 0,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  deadline_at TEXT NOT NULL,
  finished_at TEXT
);
CREATE UNIQUE INDEX agent_run_owner_key ON agent_run(epoch, owner_pseudonym, key_digest);
CREATE INDEX agent_run_owner_state ON agent_run(epoch, owner_pseudonym, state, created_at);
CREATE UNIQUE INDEX agent_run_active_lease ON agent_run(lease_token) WHERE lease_token IS NOT NULL;

CREATE TABLE agent_run_target (
  run_id TEXT NOT NULL REFERENCES agent_run(run_id) ON DELETE CASCADE,
  kind TEXT NOT NULL CHECK(kind IN ('party','opportunity','resource','material','segment')),
  target_id TEXT NOT NULL,
  PRIMARY KEY (run_id, kind, target_id)
);
CREATE INDEX agent_run_target_lookup ON agent_run_target(kind, target_id);

CREATE TABLE agent_run_turn (
  run_id TEXT NOT NULL REFERENCES agent_run(run_id) ON DELETE CASCADE,
  turn_no INTEGER NOT NULL,
  role TEXT NOT NULL CHECK(role IN ('owner','assistant','tool','system')),
  ordinal INTEGER NOT NULL,
  body TEXT NOT NULL,
  detail_json TEXT NOT NULL DEFAULT '{}',
  created_at TEXT NOT NULL,
  PRIMARY KEY (run_id, turn_no, role, ordinal)
);

CREATE TABLE agent_tool_intent (
  run_id TEXT NOT NULL REFERENCES agent_run(run_id) ON DELETE CASCADE,
  turn_no INTEGER NOT NULL,
  ordinal INTEGER NOT NULL,
  tool_name TEXT NOT NULL,
  key_digest TEXT NOT NULL,
  args_digest TEXT NOT NULL,
  state TEXT NOT NULL CHECK(state IN ('pending','dispatched','completed','failed','cancelled')),
  result_ref TEXT,
  created_at TEXT NOT NULL,
  PRIMARY KEY (run_id, turn_no, ordinal)
);

CREATE TABLE agent_run_budget (
  run_id TEXT PRIMARY KEY REFERENCES agent_run(run_id) ON DELETE CASCADE,
  invocations INTEGER NOT NULL DEFAULT 0,
  distinct_intents INTEGER NOT NULL DEFAULT 0,
  executed_writes INTEGER NOT NULL DEFAULT 0,
  destructive_effects INTEGER NOT NULL DEFAULT 0,
  updated_at TEXT NOT NULL
);
