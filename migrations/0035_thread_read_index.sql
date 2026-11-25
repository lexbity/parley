-- P7: list_threads orders by updated_at,id and is filtered by opportunity and
-- recipient. Without a leading index SQLite scans contact_thread and builds a
-- temporary B-tree for the sort on every poll. These indexes let the bounded
-- read stream in order; the pair filter already uses UNIQUE(opportunity_id,
-- recipient_id).
-- The mixed directions match `ORDER BY updated_at DESC,id` exactly; a plain
-- (updated_at,id) index still needs a temporary B-tree for the second term.
CREATE INDEX threads_by_updated ON contact_thread(updated_at DESC, id ASC);
CREATE INDEX threads_by_opportunity_updated ON contact_thread(opportunity_id, updated_at DESC, id ASC);
