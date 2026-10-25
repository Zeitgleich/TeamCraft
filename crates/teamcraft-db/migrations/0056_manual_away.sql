-- "Away" as a manual choice (docs/gateway.md, "Presence"): it stays until the user picks another
-- status, unlike the automatic away that ends with the next sign of activity.
ALTER TABLE users DROP CONSTRAINT users_presence_pref_check;
ALTER TABLE users
  ADD CONSTRAINT users_presence_pref_check CHECK (presence_pref IN ('auto', 'away', 'dnd', 'invisible'));
