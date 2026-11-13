-- Signed deletions of spaces (issue #1727, e2ee.md §4.6.5, D7). A workspace admin's device signs the
-- space with every channel it was shown; the server keeps the bytes once per space, never verifies
-- them, and hands them to the members' devices of those channels that were away. Gone, like a
-- channel's signature, with the last deleted_conversations row of its channels.
CREATE TABLE space_deletion_signatures (
  space_id   bigint      PRIMARY KEY,
  payload    bytea       NOT NULL,
  signature  bytea       NOT NULL,
  created_at timestamptz NOT NULL DEFAULT now()
);

-- Which deleted channels went with which signed space deletion: a sync names the signature to the
-- devices of a channel's members only.
CREATE TABLE space_deletion_channels (
  conversation_id bigint PRIMARY KEY,
  space_id        bigint NOT NULL REFERENCES space_deletion_signatures (space_id) ON DELETE CASCADE
);

CREATE INDEX space_deletion_channels_space_id_idx ON space_deletion_channels (space_id);
