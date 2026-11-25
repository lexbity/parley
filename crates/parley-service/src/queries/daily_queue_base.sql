WITH base AS (
  SELECT
    o.id                          AS opportunity_id,
    o.title                       AS title,
    op.party_id                   AS party_id,
    e.name                        AS recipient_name,
    coalesce(pp.do_not_contact,0) AS dnc,
    coalesce(pp.touch_count,0)    AS touches,
    coalesce(pp.highest_rung,0)   AS highest_rung,
    t.next_action_at              AS due_at,
    (SELECT m.shortlist_json
       FROM match_run m
      WHERE m.opportunity_id=o.id
        AND m.recipient_id=op.party_id
        AND m.opportunity_revision=o.revision
        AND m.recipient_revision=e.revision
      ORDER BY m.created_at DESC, m.id DESC
      LIMIT 1)                    AS shortlist_json,
    CASE
      WHEN coalesce(pp.do_not_contact,0)<>0
        OR coalesce(pp.touch_count,0)>=?1 THEN 'blocked'
      WHEN t.next_action_at IS NOT NULL THEN 'followup_due'
      ELSE 'needs_input'
    END                           AS kind
  FROM opportunity o
  JOIN opportunity_party op ON op.opportunity_id=o.id
  JOIN entity e ON e.id=op.party_id
  LEFT JOIN party_policy pp ON pp.party_id=e.id
  LEFT JOIN contact_thread t ON t.id=(
    SELECT t2.id
      FROM contact_thread t2
     WHERE t2.opportunity_id=o.id AND t2.recipient_id=e.id
     ORDER BY CASE t2.state WHEN 'waiting' THEN 0 WHEN 'conversation' THEN 1 ELSE 2 END,
              t2.created_at DESC, t2.id DESC
     LIMIT 1)
  WHERE o.state='open'
)
