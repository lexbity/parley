-- Keep the resource title canonical. The entity owns graph relationships, notes,
-- placement and focus; both records use the same immutable ID.
INSERT INTO entity(id, kind, name, revision, created_at, updated_at)
SELECT id, CASE kind WHEN 'artifact' THEN 'artifact' ELSE 'resource' END,
       '', 1, created_at, updated_at FROM resource;
CREATE TRIGGER resource_create_graph_card AFTER INSERT ON resource
BEGIN
  INSERT INTO entity(id,kind,name,revision,created_at,updated_at)
  VALUES(NEW.id,CASE NEW.kind WHEN 'artifact' THEN 'artifact' ELSE 'resource' END,
         '',1,NEW.created_at,NEW.updated_at);
END;
CREATE TRIGGER resource_update_graph_card AFTER UPDATE OF title,readiness ON resource
WHEN OLD.title<>NEW.title OR OLD.readiness<>NEW.readiness
BEGIN
  UPDATE entity SET revision=revision+1,updated_at=NEW.updated_at WHERE id=NEW.id;
END;
CREATE VIEW graph_card AS
SELECT e.id,e.kind,coalesce(r.title,e.name) name,e.focus,e.revision
FROM entity e LEFT JOIN resource r ON r.id=e.id
WHERE r.readiness IS NULL OR r.readiness<>'retired';
