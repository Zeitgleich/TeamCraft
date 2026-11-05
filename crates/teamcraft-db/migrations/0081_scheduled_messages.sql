-- Messages waiting for their time (docs/scheduled-messages.md). The ciphertext is sealed and signed by
-- the sender's device, under the content key of `mls_epoch`; the server releases it at `due_at` while
-- the conversation's MLS epoch is still that one.
CREATE TABLE scheduled_messages (
  id                 bigint PRIMARY KEY,
  user_id            bigint      NOT NULL REFERENCES users (id) ON DELETE CASCADE,
  conversation_id    bigint      NOT NULL REFERENCES conversations (id) ON DELETE CASCADE,
  client_message_id  bytea       NOT NULL,
  sender_device_id   bigint      NOT NULL,
  thread_id          bigint,
  mentioned_user_ids bigint[]    NOT NULL,
  is_urgent          boolean     NOT NULL,
  content_key_id     bytea       NOT NULL,
  ciphertext         bytea       NOT NULL,
  mls_epoch          bigint      NOT NULL CHECK (mls_epoch >= 0),
  due_at             timestamptz NOT NULL,
  -- Moves with every change, so a release that read an older row doesn't send it.
  version            bigint      NOT NULL DEFAULT 1,
  -- Why it didn't go out at its time; NULL while it waits for that time.
  hold_reason        text,
  -- A scheduler instance is on it until then; after a crash the next one takes over.
  claimed_until      timestamptz,
  -- When the sender's devices were last woken shortly before `due_at`, once per version.
  last_wake_at       timestamptz,
  created_at         timestamptz NOT NULL DEFAULT now(),
  UNIQUE (user_id, conversation_id, client_message_id)
);

CREATE INDEX scheduled_messages_due_idx ON scheduled_messages (due_at) WHERE hold_reason IS NULL;
CREATE INDEX scheduled_messages_user_idx ON scheduled_messages (user_id, due_at);
CREATE INDEX scheduled_messages_conversation_idx ON scheduled_messages (conversation_id);
