-- The shared guest link of a meeting (docs/meetings.md, "Guests"): one link for many guests. Each
-- guest who opens it gets a one-time invite of their own (`shared_invite_id` names the link), so the
-- rest of the guest flow is the same as for personal links; the guests always wait in the lobby.

ALTER TABLE meeting_invites ADD COLUMN is_shared boolean NOT NULL DEFAULT false;
-- A revoked shared link admits nobody new; a new one gets a new ID (and so a new URL).
ALTER TABLE meeting_invites ADD COLUMN revoked_at timestamptz;
ALTER TABLE meeting_invites
  ADD COLUMN shared_invite_id bigint REFERENCES meeting_invites (id) ON DELETE CASCADE;

-- At most one live shared link per meeting.
CREATE UNIQUE INDEX meeting_invites_live_shared_idx ON meeting_invites (meeting_id)
  WHERE is_shared AND revoked_at IS NULL;
-- Counting the guests of a link.
CREATE INDEX meeting_invites_shared_invite_idx ON meeting_invites (shared_invite_id)
  WHERE shared_invite_id IS NOT NULL;
