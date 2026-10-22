-- Channel moderators: keep order in a channel (remove members, delete others' posts) without
-- managing the channel itself (docs/admin.md §3.3).
ALTER TABLE memberships DROP CONSTRAINT memberships_role_check;
ALTER TABLE memberships
  ADD CONSTRAINT memberships_role_check CHECK (role IN ('admin', 'moderator', 'member'));
