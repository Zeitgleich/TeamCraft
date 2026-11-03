-- Members the import account added to a conversation it imported see the conversation's history
-- from before they joined, also as guests (e2ee.md §4.10, "Guests in imported conversations",
-- issue #1074).
ALTER TABLE memberships ADD COLUMN sees_imported_history boolean NOT NULL DEFAULT false;

-- Memberships an import before this version made: added by an account that imported, to a
-- conversation whose import was finished, from that account's first import on.
WITH importers AS (
  SELECT actor_user_id, min(created_at) AS since
  FROM audit_log
  WHERE action LIKE 'import\_%' AND actor_user_id IS NOT NULL
  GROUP BY actor_user_id
)
UPDATE memberships m SET sees_imported_history = true
FROM audit_log a
JOIN importers i ON i.actor_user_id = a.actor_user_id
JOIN conversations c ON c.id = a.target_id
WHERE a.action = 'conversation_member_added'
  AND a.target_kind = 'conversation'
  AND a.created_at >= i.since
  AND c.imported_through_message_id IS NOT NULL
  AND a.target_id = m.conversation_id
  AND a.detail->>'user_id' = m.user_id::text;
