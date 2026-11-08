-- Reminders before a planned meeting and the notice at its start come from the server at the
-- scheduled time, as a normal push, not a ring (#1421, docs/meetings.md "Starting").

-- The user's lead time in minutes (0: no reminder before the meeting) and whether the start
-- notifies. NULL: never chose; the defaults are 5 minutes and yes.
ALTER TABLE user_notification_settings
  ADD COLUMN meeting_reminder_minutes smallint
    CHECK (meeting_reminder_minutes BETWEEN 0 AND 60),
  ADD COLUMN notifies_meeting_start boolean;

-- Which notice went to whom, so that a restart or a second server instance doesn't send it again.
-- One row per user, occurrence and kind; nothing here is planned ahead: what is due is worked out
-- from the meeting as it is now, so a meeting that moved or was cancelled can't fire.
CREATE TABLE meeting_notices (
  meeting_id bigint      NOT NULL REFERENCES meetings (id) ON DELETE CASCADE,
  starts_at  timestamptz NOT NULL,
  kind       text        NOT NULL CHECK (kind IN ('reminder', 'start')),
  user_id    bigint      NOT NULL REFERENCES users (id) ON DELETE CASCADE,
  PRIMARY KEY (meeting_id, starts_at, kind, user_id)
);
CREATE INDEX meeting_notices_starts_at_idx ON meeting_notices (starts_at);
CREATE INDEX meeting_notices_user_idx ON meeting_notices (user_id);

-- A push waiting for a retry keeps which meeting and occurrence it tells about.
ALTER TABLE push_retries
  ADD COLUMN meeting_id bigint,
  ADD COLUMN meeting_starts_at_ms bigint;
