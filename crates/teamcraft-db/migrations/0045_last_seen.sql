-- Last seen (PLAN.md §4): when the user was last active on any device, and who may see it.
-- Presence is metadata the server needs anyway; this is one more timestamp, no content.
ALTER TABLE users
  ADD COLUMN last_seen_at         timestamptz,
  ADD COLUMN last_seen_visibility text NOT NULL DEFAULT 'everyone'
    CHECK (last_seen_visibility IN ('everyone', 'nobody'));
