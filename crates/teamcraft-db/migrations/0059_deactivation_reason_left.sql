-- A meeting guest who leaves is deactivated with the reason 'left' (docs/meetings.md, "Guests in
-- the browser").
ALTER TABLE users DROP CONSTRAINT users_deactivation_reason_check;
ALTER TABLE users
  ADD CONSTRAINT users_deactivation_reason_check CHECK (deactivation_reason IN ('admin', 'oidc', 'left'));
