-- Space-level notification settings (PLAN.md §1, "mute whole space"). A channel inherits the
-- member's setting for its space unless the member set the channel's own level or mute time.

ALTER TABLE memberships
  -- true once the member chose a level or mute time for this conversation themselves.
  ADD COLUMN notification_overridden boolean NOT NULL DEFAULT false;
-- Settings made before the space level existed count as the member's own choice.
UPDATE memberships SET notification_overridden = true
  WHERE notification_level <> 'mentions' OR muted_until IS NOT NULL;

ALTER TABLE space_members
  -- NULL: no space-level choice, the channels' own levels apply.
  ADD COLUMN notification_level text CHECK (notification_level IN ('all', 'mentions', 'none')),
  ADD COLUMN muted_until timestamptz;
