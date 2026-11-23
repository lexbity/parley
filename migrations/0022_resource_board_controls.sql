-- R01: Board controls amendment — only Inbox stays a system list.
--
-- The board seed marked Inbox, Developing and Available `kind='system'`.
-- Owner-managed lists need no extra confirmation, so Developing and
-- Available become ordinary owner lists; workspaces created before this
-- amendment get the same treatment as fresh seeds. Inbox remains `system`:
-- removing it demands an explicit extra confirmation because the seed
-- never recreates it.

UPDATE resource_column SET kind='owner' WHERE id IN ('developing','available');
