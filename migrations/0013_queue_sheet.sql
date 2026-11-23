-- S12: Queue sheet phase model (Parley-Queue implementation spec).
--
-- The Queue is a filterable sheet of opportunities. Each row carries a pack
-- stage (phase) that the owner may change through a typed, revision-guarded
-- command. Lifecycle (opportunity.state), recipient/thread state and next
-- action stay separate fields; a manual phase change alone never synthesizes
-- a send, reply, benefit or ladder advance.
--
-- Pack stages are canonical records scoped to a pack id/version. The default
-- job_search@1.0.0 pack is seeded with the planning stages used by the queue
-- study; prototype labels are illustrative, never inferred from English.

CREATE TABLE pack_stage (
  id TEXT PRIMARY KEY,
  pack_id TEXT NOT NULL,
  pack_version TEXT NOT NULL,
  stage_id TEXT NOT NULL,
  label TEXT NOT NULL,
  class TEXT NOT NULL,
  kind TEXT NOT NULL CHECK(kind IN ('planning','terminal')),
  ordinal INTEGER NOT NULL,
  UNIQUE(pack_id, pack_version, stage_id)
);
CREATE INDEX pack_stage_active ON pack_stage(pack_id, pack_version, ordinal);

INSERT INTO pack_stage(id,pack_id,pack_version,stage_id,label,class,kind,ordinal) VALUES
  ('job_search@1.0.0@exploring','job_search','1.0.0','exploring','Exploring','identified','planning',10),
  ('job_search@1.0.0@preparing','job_search','1.0.0','preparing','Preparing','prepared','planning',20),
  ('job_search@1.0.0@in_conversation','job_search','1.0.0','in_conversation','In conversation','responded','planning',30),
  ('job_search@1.0.0@closed','job_search','1.0.0','closed','Closed','closed','terminal',40);

-- Phase fields on opportunity. `stage_id` is NULL for opportunities that
-- predate the phase model (they are "Unmapped" until the owner maps them
-- explicitly; history is never invented). phase_revision is the concurrency
-- token for the phase edit; last_phase_event_id anchors ordinary Undo.
ALTER TABLE opportunity ADD COLUMN stage_id TEXT;
ALTER TABLE opportunity ADD COLUMN stage_pack_id TEXT;
ALTER TABLE opportunity ADD COLUMN stage_pack_version TEXT;
ALTER TABLE opportunity ADD COLUMN stage_class TEXT;
ALTER TABLE opportunity ADD COLUMN phase_revision INTEGER NOT NULL DEFAULT 1 CHECK(phase_revision > 0);
ALTER TABLE opportunity ADD COLUMN last_phase_event_id TEXT;

CREATE INDEX opportunity_phase ON opportunity(stage_id, phase_revision, id);
CREATE INDEX opportunity_phase_pack ON opportunity(stage_pack_id, stage_pack_version, stage_id);
