-- Deleting a user (admin.md §3.2.1, issue #630). The row stays as a tombstone because messages,
-- reactions, the audit log and call history reference it; `deleted_at` marks it. A deleted user is
-- always deactivated as well, so every check that refuses deactivated accounts refuses it too.
ALTER TABLE users ADD COLUMN deleted_at timestamptz;
ALTER TABLE users
  ADD CONSTRAINT users_deleted_is_deactivated CHECK (deleted_at IS NULL OR deactivated_at IS NOT NULL);

-- Direct messages deleted with a user, per remaining participant: a device that was offline when
-- the gateway announced it drops the conversation and its local history on the next full sync.
-- Kept for a while (the housekeeping prunes old rows), then such a device only drops the listing.
CREATE TABLE deleted_conversations (
  user_id         bigint      NOT NULL REFERENCES users (id) ON DELETE CASCADE,
  conversation_id bigint      NOT NULL,
  deleted_at      timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (user_id, conversation_id)
);
CREATE INDEX deleted_conversations_deleted_at_idx ON deleted_conversations (deleted_at);

-- The admin console's storage column sums each channel's attachments.
CREATE INDEX attachments_conversation_idx ON attachments (conversation_id) WHERE conversation_id IS NOT NULL;
