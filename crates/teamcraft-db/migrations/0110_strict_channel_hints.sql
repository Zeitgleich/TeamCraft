-- What a channel's members told the server about its strict roles (e2ee.md §4.6.2, issue #1924):
-- whether it is strict and the version and chain hash of the roles change that made it so. A hint
-- for the UI and for a workspace admin outside the group who signs the next change; the group
-- context alone switches strict, and no member decides anything on these columns. Set with the
-- roles commit that carries them, in the commit's transaction.
ALTER TABLE conversations
  ADD COLUMN is_strict            boolean NOT NULL DEFAULT false,
  ADD COLUMN strict_roles_version bigint,
  ADD COLUMN strict_roles_hash    bytea;
