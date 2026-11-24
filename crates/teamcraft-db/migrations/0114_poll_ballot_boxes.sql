-- Ballot boxes of anonymous polls (issue #2121, e2ee.md §4.12.1).
--
-- A ballot is sealed under a key only the conversation's members hold; the server keeps it with its
-- voter only while the box is open, to keep one ballot per account. Closing erases the voters.

-- One box per anonymous poll, opened by the poll's sender. No foreign key to the poll's message
-- (`messages` is partitioned): retention deletes the box with the message.
CREATE TABLE poll_ballot_boxes (
  conversation_id  bigint      NOT NULL REFERENCES conversations (id) ON DELETE CASCADE,
  message_id       bigint      NOT NULL,
  creator_user_id  bigint      NOT NULL,
  closes_at        timestamptz,
  closed_at        timestamptz,
  created_at       timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (conversation_id, message_id)
);
CREATE INDEX poll_ballot_boxes_due_idx ON poll_ballot_boxes (closes_at)
  WHERE closed_at IS NULL AND closes_at IS NOT NULL;

CREATE TABLE poll_ballots (
  id               bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
  conversation_id  bigint NOT NULL,
  message_id       bigint NOT NULL,
  -- NULL once the box closed (or the account was deleted): nobody can tell whose ballot it was.
  voter_user_id    bigint REFERENCES users (id) ON DELETE SET NULL,
  ballot           bytea  NOT NULL CHECK (octet_length(ballot) <= 64),
  FOREIGN KEY (conversation_id, message_id)
    REFERENCES poll_ballot_boxes (conversation_id, message_id) ON DELETE CASCADE,
  UNIQUE (conversation_id, message_id, voter_user_id)
);
