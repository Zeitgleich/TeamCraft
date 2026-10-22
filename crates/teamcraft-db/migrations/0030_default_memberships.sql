-- When a user received the instance's default spaces and channels (policy `default_spaces`). It is
-- claimed once, so people who later leave a default space are not added again.
ALTER TABLE users ADD COLUMN default_memberships_at timestamptz;
-- Existing accounts are not "new": they keep exactly the memberships they have.
UPDATE users SET default_memberships_at = now();
