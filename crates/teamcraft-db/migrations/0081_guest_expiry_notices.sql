-- Guests hear before their membership runs out, and so does their sponsor (issue #212). The lead
-- time (in days) of the last notice sent for the current expiry; a new expiry starts over.
ALTER TABLE space_members ADD COLUMN expiry_notice_lead_days integer;

CREATE INDEX space_members_guest_expiry_idx ON space_members (expires_at)
  WHERE role = 'guest' AND expires_at IS NOT NULL;
