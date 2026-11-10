-- Accounts listed and addable before their first sign-in (#1632, docs/provisioning.md §6). The
-- identity provider sync and the import create accounts from the provider before the person ever
-- signed in. Such an account carries the time it was made until the first sign-in clears it: the
-- directory marks it "not yet signed in", and a provider that disables or deletes it meanwhile has
-- it removed outright instead of deactivated (nothing to rotate, nobody to suspend).
ALTER TABLE users ADD COLUMN awaiting_first_sign_in_since timestamptz;

-- Accounts an import made before this column: provider members whose default memberships (applied
-- at the first sign-in) never were, and who never had a device, a session or a presence.
UPDATE users u SET awaiting_first_sign_in_since = u.created_at
WHERE u.oidc_issuer IS NOT NULL
  AND u.oidc_subject IS NOT NULL
  AND u.deleted_at IS NULL
  AND u.kind = 'member'
  AND u.default_memberships_at IS NULL
  AND u.last_seen_at IS NULL
  AND NOT EXISTS (SELECT 1 FROM devices d WHERE d.user_id = u.id)
  AND NOT EXISTS (
    SELECT 1 FROM session_activity s WHERE s.issuer = u.oidc_issuer AND s.subject = u.oidc_subject
  );

-- The sync's run report counts what it created and removed outright.
ALTER TABLE oidc_sync_status
  ADD COLUMN created integer NOT NULL DEFAULT 0,
  ADD COLUMN deleted integer NOT NULL DEFAULT 0;
