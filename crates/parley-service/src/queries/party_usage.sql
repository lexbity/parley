-- Party usage: two indexed probes (match_run recipient + normalized
-- shortlist mentions) deduplicated by UNION. Replaces the retired json_tree
-- scan of every match_run row.
SELECT count(*) FROM (
  SELECT id FROM match_run WHERE recipient_id=?
  UNION
  SELECT match_run_id FROM match_run_mention WHERE entity_id=?
)
