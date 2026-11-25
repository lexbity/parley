, numbered AS (
  SELECT base.*,
         row_number() OVER (PARTITION BY kind ORDER BY opportunity_id, party_id) AS rn
    FROM base
)
SELECT opportunity_id,title,party_id,recipient_name,dnc,touches,highest_rung,
       due_at,shortlist_json,kind
  FROM numbered
 WHERE rn <= ?2
 ORDER BY opportunity_id, party_id
