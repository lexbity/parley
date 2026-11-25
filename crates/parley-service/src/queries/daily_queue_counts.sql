SELECT
  coalesce(sum(CASE WHEN kind<>'blocked' THEN 1 ELSE 0 END),0) AS total_actionable,
  coalesce(sum(CASE WHEN kind='blocked' THEN 1 ELSE 0 END),0)  AS total_blocked,
  coalesce(sum(CASE WHEN kind IN ('followup_due','ready_to_send') THEN 1 ELSE 0 END),0) AS ready_count
FROM base
