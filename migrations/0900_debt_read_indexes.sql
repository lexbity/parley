-- TD-001 / TD-04: bounded-read indexes and match-run mention normalization.
-- Allocation: out-of-plan reserved band [0900,1000) (TD-001/TD-09). Every
-- statement is additive, forward-only and online in SQLite; no data is
-- rewritten except the one-time, idempotent mention backfill below.

CREATE INDEX IF NOT EXISTS event_entity_cursor
  ON event(entity_id, cursor);
CREATE INDEX IF NOT EXISTS event_type_cursor
  ON event(event_type, cursor);
CREATE INDEX IF NOT EXISTS edge_evidence_edge
  ON edge_evidence(edge_id);
CREATE INDEX IF NOT EXISTS match_run_recipient
  ON match_run(recipient_id);
CREATE INDEX IF NOT EXISTS material_updated
  ON material(updated_at, id);
-- The shipped column is `lease_deadline` (0022_agent_runs.sql), not
-- `lease_deadline_at` as named in the TD-001 draft.
CREATE INDEX IF NOT EXISTS agent_run_lease_expiry
  ON agent_run(state, lease_deadline);

-- A shortlist can mention parties other than the run's recipient. Those
-- mentions are normalized at write time so party usage can be counted by two
-- indexed probes instead of a json_tree scan of every match_run row. The
-- parent is match_run(id): in this tree match_run ids are derived from their
-- driving inference job (`{job_id}:match`) and are not themselves rows in
-- inference_record.
CREATE TABLE match_run_mention (
  match_run_id TEXT NOT NULL REFERENCES match_run(id) ON DELETE CASCADE,
  entity_id    TEXT NOT NULL,
  PRIMARY KEY (match_run_id, entity_id)
);
CREATE INDEX match_run_mention_entity ON match_run_mention(entity_id);

-- One bounded pass over existing runs. The extraction rule is identical to
-- the old usage query: every text atom of shortlist_json is a candidate
-- mention. ON CONFLICT makes the backfill safe to re-run and keeps the
-- migration idempotent under a partially applied upgrade.
INSERT INTO match_run_mention(match_run_id, entity_id)
SELECT DISTINCT m.id, j.atom
  FROM match_run m, json_tree(m.shortlist_json) j
 WHERE j.type='text' AND j.atom IS NOT NULL
ON CONFLICT(match_run_id, entity_id) DO NOTHING;
