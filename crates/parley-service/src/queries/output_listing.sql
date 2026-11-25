-- Aggregate registry projection: one statement, no bodies. The caller appends
-- the keyset predicate, ORDER BY and LIMIT through QueryBuilder with bound
-- values; no client text is interpolated.
WITH approval_snapshot AS (
  SELECT material_id,version_no,gate_snapshot_json FROM approval
),
latest_release AS (
  SELECT material_id,version_no,max(issued_at) AS issued_at
    FROM release_record
   GROUP BY material_id,version_no
)
SELECT m.id,m.opportunity_id,m.recipient_id,e.name,m.kind,mv.version_no,
       mv.content_sha256,
       CASE WHEN r.issued_at IS NOT NULL THEN 'released'
            WHEN a.gate_snapshot_json IS NOT NULL THEN 'approved'
            ELSE 'draft' END AS status,
       a.gate_snapshot_json,mv.blob_upload_id,mv.blob_sha256,mv.filename,
       mv.content_type,mv.attachment_size,m.updated_at
  FROM material m
  JOIN entity e ON e.id=m.recipient_id
  JOIN material_version mv ON mv.material_id=m.id AND mv.version_no=m.head_version
  LEFT JOIN approval_snapshot a ON a.material_id=m.id AND a.version_no=mv.version_no
  LEFT JOIN latest_release r ON r.material_id=m.id AND r.version_no=mv.version_no
 WHERE 1=1
