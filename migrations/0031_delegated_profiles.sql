-- AR-001 AR05: pin every delegated run to an immutable qualified profile and
-- a canonical transcript version, and journal ordered host-owned tool calls.
-- Pre-Pi in-flight runs cannot resume under the new protocol: they receive a
-- stable terminal disposition while their committed receipts and history stay.

ALTER TABLE agent_run ADD COLUMN profile_revision INTEGER;
ALTER TABLE agent_run ADD COLUMN transcript_major INTEGER NOT NULL DEFAULT 1;
ALTER TABLE agent_run ADD COLUMN process_nonce TEXT;
ALTER TABLE agent_run ADD COLUMN wait_intent TEXT;
ALTER TABLE agent_run ADD COLUMN resume_count INTEGER NOT NULL DEFAULT 0;

ALTER TABLE agent_tool_intent ADD COLUMN call_ref TEXT;
ALTER TABLE agent_tool_intent ADD COLUMN accepted_at TEXT;

UPDATE agent_run
   SET state='failed',
       wake_condition='INTERRUPTED_UPGRADE',
       lease_token=NULL,
       lease_deadline=NULL,
       finished_at=COALESCE(finished_at, updated_at),
       updated_at=updated_at
 WHERE state IN ('queued', 'running', 'waiting_job', 'waiting_owner');

CREATE INDEX agent_run_delegated_ready
    ON agent_run(state, epoch, updated_at, run_id)
    WHERE state IN ('queued', 'waiting_job', 'waiting_owner');

CREATE INDEX agent_tool_intent_accepted
    ON agent_tool_intent(run_id, turn_no, ordinal)
    WHERE accepted_at IS NOT NULL;
