-- Issue #1441: an organizer's own holiday region, and the holidays each series' calendar event
-- was last written with, so the server can tell which events to write again when the holidays
-- change (a new feed copy, other settings, an organizer's new region, time moving on).

-- One of the regions of the bundled holiday table (checked by the server); NULL: the instance's.
ALTER TABLE users ADD COLUMN holiday_region text;

-- The starts the calendar event of a series leaves out as public holidays (its holiday EXDATEs),
-- sorted, as of the last write of the event; NULL: not known (written by an older version, or
-- never).
ALTER TABLE meetings ADD COLUMN calendar_holidays timestamptz[];

-- The series whose events the holiday job looks at, in the order it walks them.
CREATE INDEX meetings_skipping_holidays ON meetings (id)
  WHERE skips_holidays AND recurrence_rule IS NOT NULL;
