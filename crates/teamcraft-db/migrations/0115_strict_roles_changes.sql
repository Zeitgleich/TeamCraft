-- A strict channel's signed roles changes as its members' commits carried them (e2ee.md §4.6.2,
-- "Falling out in a strict channel", issue #2251): the committer sends the signed change with the
-- commit's strict hint, and the server keeps it here, under the hint's version, in the commit's
-- transaction. A version can hold several, one per commit (the change the group took, ones it
-- refused, a member's junk): none blocks another. A device that fell out reads the changes after
-- the version it knew and verifies the chain itself; the server never decides on them. A rewind
-- drops the commits it rewinds, and with them the changes they carried, so the chain never holds a
-- change the group no longer has.
CREATE TABLE mls_strict_changes (
  conversation_id bigint NOT NULL REFERENCES conversations (id) ON DELETE CASCADE,
  version         bigint NOT NULL CHECK (version >= 0),
  mls_message_id  bigint NOT NULL REFERENCES mls_messages (id) ON DELETE CASCADE,
  change          bytea  NOT NULL,
  PRIMARY KEY (conversation_id, version, mls_message_id)
);
-- The cascade from a dropped commit looks the changes up by it.
CREATE INDEX mls_strict_changes_message_idx ON mls_strict_changes (mls_message_id);
