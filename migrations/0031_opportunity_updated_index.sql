-- P4: the opportunity sheet orders its unbounded default read by
-- o.updated_at,o.id. Without an index SQLite scans the table and builds a
-- temporary B-tree for the sort. The index lets that read stream in order.
CREATE INDEX opportunity_updated ON opportunity(updated_at, id);
