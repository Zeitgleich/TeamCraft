-- "Busy" and "Be right back" as manual choices (issue #1578). `dnd_until` now ends any manual
-- status, not only do-not-disturb; the server returns to automatic once it has passed.
ALTER TABLE users DROP CONSTRAINT users_presence_pref_check;
ALTER TABLE users
  ADD CONSTRAINT users_presence_pref_check
  CHECK (presence_pref IN ('auto', 'away', 'busy', 'brb', 'dnd', 'invisible'));
