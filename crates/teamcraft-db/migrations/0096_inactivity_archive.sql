-- Inactive channels, archived with their space, idle guest accounts (issue #212). Metadata only, and
-- additive: nothing here changes what an instance does until the signed policy turns it on.

-- Why a channel is archived, so that reverting an operation restores exactly what it archived and the
-- retention step deletes only what the inactivity policy archived itself. NULL: by hand, or before
-- this column existed.
ALTER TABLE conversations
  ADD COLUMN archive_origin text CHECK (archive_origin IN ('space', 'inactivity')),
  -- When the owner notice of the current inactive spell went out; for an archived channel, when the
  -- notice before its deletion did. Cleared by a snooze and by any new activity.
  ADD COLUMN inactivity_notified_at timestamptz,
  -- Nothing is noticed, archived or deleted before this.
  ADD COLUMN inactivity_snoozed_until timestamptz;

CREATE INDEX conversations_archive_origin_idx ON conversations (archived_at)
  WHERE archive_origin = 'inactivity' AND deleted_at IS NULL;

-- Since when a guest account has belonged to no space (set by the guest housekeeping while the
-- policy has a period for it, cleared when the guest is in a space again).
ALTER TABLE users ADD COLUMN memberless_since timestamptz;
