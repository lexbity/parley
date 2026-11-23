-- Legacy inventory keeps its IDs and existing material references. Populate
-- the corresponding cards so an off-board resource can be opened and placed.
UPDATE entity
SET name = (SELECT title FROM resource WHERE id = entity.id),
    subtype = (SELECT kind FROM resource WHERE id = entity.id),
    body_json = (SELECT body_json FROM resource WHERE id = entity.id),
    readiness = (SELECT readiness FROM resource WHERE id = entity.id)
WHERE id IN (SELECT id FROM resource) AND subtype = '';

DROP TRIGGER resource_create_graph_card;
CREATE TRIGGER resource_create_graph_card AFTER INSERT ON resource
BEGIN
  INSERT INTO entity(id,kind,name,revision,created_at,updated_at,subtype,body_json,readiness)
  VALUES(NEW.id,CASE NEW.kind WHEN 'artifact' THEN 'artifact' ELSE 'resource' END,
         NEW.title,NEW.revision,NEW.created_at,NEW.updated_at,NEW.kind,NEW.body_json,NEW.readiness);
END;

DROP TRIGGER resource_update_graph_card;
CREATE TRIGGER resource_update_graph_card AFTER UPDATE OF title,body_json,readiness ON resource
BEGIN
  UPDATE entity SET name=NEW.title,body_json=NEW.body_json,readiness=NEW.readiness,
                    revision=max(revision+1,NEW.revision),updated_at=NEW.updated_at
  WHERE id=NEW.id AND (name<>NEW.title OR body_json<>NEW.body_json OR readiness<>NEW.readiness);
END;

DROP TRIGGER resource_placement_insert_guard;
DROP TRIGGER resource_placement_update_guard;
CREATE TRIGGER resource_placement_insert_guard
BEFORE INSERT ON resource_placement
WHEN NOT EXISTS (SELECT 1 FROM entity WHERE id=NEW.resource_id AND subtype<>'' AND retired_at IS NULL AND readiness<>'retired')
BEGIN SELECT RAISE(ABORT, 'placement requires an active resource'); END;
CREATE TRIGGER resource_placement_update_guard
BEFORE UPDATE OF resource_id ON resource_placement
WHEN NOT EXISTS (SELECT 1 FROM entity WHERE id=NEW.resource_id AND subtype<>'' AND retired_at IS NULL AND readiness<>'retired')
BEGIN SELECT RAISE(ABORT, 'placement requires an active resource'); END;

DROP VIEW graph_card;
CREATE VIEW graph_card AS
SELECT e.id,e.kind,coalesce(r.title,e.name) name,e.focus,e.revision
FROM entity e LEFT JOIN resource r ON r.id=e.id
WHERE e.retired_at IS NULL AND e.readiness<>'retired' AND (r.readiness IS NULL OR r.readiness<>'retired');

ALTER TABLE edge_history ADD COLUMN basis TEXT NOT NULL DEFAULT '';

-- Snapshot the resource whose exact attachment upload was included in a
-- material version. Drafting dependencies and equal bytes from another
-- upload do not establish inclusion. Unlinking a file preserves this snapshot.
CREATE TABLE delivered_resource_version (
    resource_id TEXT NOT NULL REFERENCES entity(id) ON DELETE CASCADE,
    material_id TEXT NOT NULL,
    version_no INTEGER NOT NULL,
    resource_revision INTEGER NOT NULL,
    resource_title TEXT NOT NULL,
    PRIMARY KEY (resource_id, material_id, version_no),
    FOREIGN KEY (material_id, version_no) REFERENCES material_version(material_id, version_no) ON DELETE CASCADE
);
CREATE INDEX material_included_resources ON delivered_resource_version(material_id, version_no);
CREATE INDEX send_parts_by_version ON send_part(material_id, version_no, send_id);
CREATE INDEX active_edges_by_target ON edge(to_id, from_id) WHERE state='active';

CREATE TRIGGER material_version_snapshot_resource_attachment
AFTER INSERT ON material_version
WHEN NEW.blob_upload_id IS NOT NULL
BEGIN
  INSERT INTO delivered_resource_version(resource_id,material_id,version_no,resource_revision,resource_title)
  SELECT e.id,NEW.material_id,NEW.version_no,e.revision,e.name
  FROM entity_attachment a JOIN entity e ON e.id=a.entity_id
  WHERE a.upload_id=NEW.blob_upload_id AND e.subtype<>'' AND e.readiness<>'retired';
END;

-- Historical resources may have been edited or unlinked after a send. There
-- is no recorded binding revision to backfill for those material versions.
