-- Legal hold on people and members' own deletions under a hold (issue #2135, data-export.md §4.2).
--
-- The signed policy is the authority; these tables are what the server derives from its newest entry,
-- written in the transaction that stores that entry, so a deletion that reads them under its locks
-- sees the same holds as the policy it reads there.

-- The people the newest policy holds, with the scope of each hold.
CREATE TABLE legal_hold_people (
  user_id      bigint  PRIMARY KEY REFERENCES users (id) ON DELETE CASCADE,
  -- `direct`: only their direct messages and groups; otherwise every conversation they are in.
  direct_only  boolean NOT NULL DEFAULT false
);

-- Conversations a held person left while held: still covered by their hold until it is released.
-- No foreign key: a conversation deleted later just leaves a row nobody reads, removed on release.
CREATE TABLE legal_hold_departures (
  conversation_id  bigint      NOT NULL,
  user_id          bigint      NOT NULL,
  left_at          timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (user_id, conversation_id)
);
CREATE INDEX legal_hold_departures_conversation_idx ON legal_hold_departures (conversation_id);

CREATE FUNCTION tc_record_legal_hold_departure() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
  INSERT INTO legal_hold_departures (conversation_id, user_id)
    SELECT OLD.conversation_id, OLD.user_id
    WHERE EXISTS (SELECT 1 FROM legal_hold_people p WHERE p.user_id = OLD.user_id)
  ON CONFLICT DO NOTHING;
  RETURN NULL;
END;
$$;

CREATE TRIGGER memberships_legal_hold_departure AFTER DELETE ON memberships
  FOR EACH ROW EXECUTE FUNCTION tc_record_legal_hold_departure();

-- The conversations a person hold covers: those a held person is a member of, the direct messages
-- and groups they were opened for, and those they left while held. Meeting chats go with their
-- meeting and are never covered; a `direct_only` hold covers direct messages and groups only.
CREATE VIEW legal_hold_covered AS
  SELECT c.id AS conversation_id, p.user_id
    FROM legal_hold_people p
    JOIN memberships m ON m.user_id = p.user_id
    JOIN conversations c ON c.id = m.conversation_id
    WHERE c.kind IN ('dm', 'group') OR (c.kind = 'channel' AND NOT p.direct_only)
  UNION
  SELECT c.id, p.user_id
    FROM legal_hold_people p
    JOIN conversations c
      ON c.participants_key IS NOT NULL
     AND p.user_id = ANY (string_to_array(c.participants_key, '-')::bigint[])
  UNION
  SELECT c.id, p.user_id
    FROM legal_hold_people p
    JOIN legal_hold_departures d ON d.user_id = p.user_id
    JOIN conversations c ON c.id = d.conversation_id
    WHERE c.kind IN ('dm', 'group') OR (c.kind = 'channel' AND NOT p.direct_only);

-- A member's signed deletion of their own message in a held conversation: the ciphertext stays,
-- marked deleted and hidden from clients, until the hold is released and the server removes it.
ALTER TABLE messages ADD COLUMN deleted_at timestamptz;
CREATE INDEX messages_pending_deletion_idx ON messages (conversation_id) WHERE deleted_at IS NOT NULL;
