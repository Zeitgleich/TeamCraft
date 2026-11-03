-- Signed deletions of channels (issue #1135, e2ee.md §4.6). The signature an admin's device made, kept
-- as bytes for the members' devices that were away: the server never verifies it. One per conversation,
-- gone with the last of its deleted_conversations rows.
CREATE TABLE conversation_deletion_signatures (
  conversation_id bigint      PRIMARY KEY,
  payload         bytea       NOT NULL,
  signature       bytea       NOT NULL,
  created_at      timestamptz NOT NULL DEFAULT now()
);

-- A device that was away verifies such a deletion against the channel's roles, and may have to read
-- role entries it missed. So a signed-deleted channel's log outlives the channel's row, until its
-- signature goes; every other removal of a conversation clears the log itself.
ALTER TABLE channel_role_log DROP CONSTRAINT channel_role_log_conversation_id_fkey;
