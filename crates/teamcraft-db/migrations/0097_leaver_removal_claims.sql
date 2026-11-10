-- Leavers leave the encryption groups automatically (issue #210, docs/provisioning.md §3): an owner's
-- or admin's app claims a leaver before it signs their removals, so two devices don't do it at once.
-- A claim is a lease: one that ran out (the app crashed) is taken over by the next device.
CREATE TABLE leaver_removal_claims (
  user_id       bigint      PRIMARY KEY REFERENCES users (id) ON DELETE CASCADE,
  device_id     bigint      NOT NULL,
  claimed_until timestamptz NOT NULL
);

-- Who is a leaver, in one place: deactivated by the identity provider, not deleted, not a bot, and
-- still in a conversation or space or with a device.
CREATE VIEW identity_provider_leavers AS
  SELECT u.id AS user_id, u.deactivated_at
  FROM users u
  WHERE u.deactivated_at IS NOT NULL AND u.deleted_at IS NULL AND u.kind <> 'bot'
    AND u.deactivation_reason = 'oidc'
    AND (EXISTS (SELECT 1 FROM memberships m WHERE m.user_id = u.id)
      OR EXISTS (SELECT 1 FROM space_members m WHERE m.user_id = u.id)
      OR EXISTS (SELECT 1 FROM devices d WHERE d.user_id = u.id AND d.revoked_at IS NULL));
