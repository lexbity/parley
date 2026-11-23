ALTER TABLE entity ADD COLUMN retired_at TEXT;
DROP VIEW graph_card;
CREATE VIEW graph_card AS
SELECT e.id,e.kind,coalesce(r.title,e.name) name,e.focus,e.revision
FROM entity e LEFT JOIN resource r ON r.id=e.id
WHERE e.retired_at IS NULL AND (r.readiness IS NULL OR r.readiness<>'retired');
