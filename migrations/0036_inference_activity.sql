-- AR-001 AR09. Owner Activity: one bounded union read over durable role jobs
-- and delegated runs, sorted by (updated_at DESC, kind DESC, id DESC) with a
-- signed keyset cursor. These indexes let either side stream in exactly that
-- order so a page never scans the whole history and activity reads stay
-- bounded while manual work continues.

CREATE INDEX inference_job_activity
    ON durable_job(epoch, updated_at DESC, id);

CREATE INDEX agent_run_activity
    ON agent_run(epoch, owner_pseudonym, updated_at DESC, run_id);
