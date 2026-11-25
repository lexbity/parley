-- P8: GET /outputs orders the registry by updated_at,id. The existing
-- material_opportunity_contact index starts with opportunity_id, so a global
-- registry read still scans and sorts. The mixed directions match the exact
-- ORDER BY; no temporary B-tree is needed for the second term.
CREATE INDEX material_updated ON material(updated_at DESC, id ASC);
