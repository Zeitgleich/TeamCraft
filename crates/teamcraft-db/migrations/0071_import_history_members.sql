-- Members the import account added while it imported see the conversation's history from before
-- they joined, also as guests (e2ee.md §4.10, "Guests in imported conversations", issue #1074).
ALTER TABLE memberships ADD COLUMN sees_imported_history boolean NOT NULL DEFAULT false;

-- Memberships an import before this version made: added by an account that imported.
UPDATE memberships m SET sees_imported_history = true
FROM audit_log a
WHERE a.action = 'conversation_member_added'
  AND a.target_kind = 'conversation'
  AND a.target_id = m.conversation_id
  AND a.detail->>'user_id' = m.user_id::text
  AND a.actor_user_id IN (SELECT actor_user_id FROM audit_log WHERE action LIKE 'import\_%');
