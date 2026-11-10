-- A meeting notice dealt with on one device is taken down on the user's other phones (#1473). The
-- time a notice was reported as dealt with makes a second report do nothing, also when it comes
-- from two devices at once.
ALTER TABLE meeting_notices
  ADD COLUMN dismissed_at timestamptz;
