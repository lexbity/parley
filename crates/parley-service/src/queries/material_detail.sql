-- Head material detail: the only route that serves letter bodies.
SELECT m.id,m.opportunity_id,m.recipient_id,e.name,m.kind,mv.version_no,
       mv.body,mv.content_sha256,mv.blob_upload_id,mv.blob_sha256,mv.filename,
       mv.content_type,mv.attachment_size
  FROM material m
  JOIN entity e ON e.id=m.recipient_id
  JOIN material_version mv ON mv.material_id=m.id AND mv.version_no=m.head_version
 WHERE m.id=?
