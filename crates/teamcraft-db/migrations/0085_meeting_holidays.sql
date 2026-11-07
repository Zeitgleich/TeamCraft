-- Issue #1053: a repeating meeting can skip public holidays by itself. The holidays come from the
-- instance's region (admin console); the occurrences on them are left out like cancelled ones, and
-- the organizer can keep one by restoring it.

-- The series leaves out its occurrences on public holidays.
ALTER TABLE meetings ADD COLUMN skips_holidays boolean NOT NULL DEFAULT false;

-- An occurrence on a public holiday the organizer keeps, by the start it has in the series (its
-- iCalendar RECURRENCE-ID). Bounded per series by the service, like cancelled occurrences.
CREATE TABLE meeting_kept_holidays (
  meeting_id bigint      NOT NULL REFERENCES meetings (id) ON DELETE CASCADE,
  starts_at  timestamptz NOT NULL,
  kept_at    timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (meeting_id, starts_at)
);
