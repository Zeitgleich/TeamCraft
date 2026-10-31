-- Issue #1006: one occurrence of a repeating meeting can be cancelled (and restored) without the
-- series; a cancelled meeting is listed once, then can be taken off the lists.

-- A cancelled occurrence, by the start it has in the series (its iCalendar RECURRENCE-ID).
CREATE TABLE meeting_cancelled_occurrences (
  meeting_id   bigint      NOT NULL REFERENCES meetings (id) ON DELETE CASCADE,
  starts_at    timestamptz NOT NULL,
  cancelled_at timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (meeting_id, starts_at)
);

-- When the whole meeting was cancelled (NULL for single meetings cancelled before this change), and
-- when its organizer took it off the lists.
ALTER TABLE meetings
  ADD COLUMN cancelled_at timestamptz,
  ADD COLUMN hidden_at timestamptz;

-- Ending the call of a repeating meeting for everyone marked the whole series ended, and a cancelled
-- series stayed a dead entry every day: such series are on again, and cancelled ones are listed
-- once more (at their next occurrence) so their organizer can restore or remove them.
UPDATE meetings SET status = 'scheduled' WHERE status = 'ended' AND recurrence_rule IS NOT NULL;
UPDATE meetings SET cancelled_at = now() WHERE status = 'cancelled' AND recurrence_rule IS NOT NULL;
