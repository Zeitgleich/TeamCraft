-- Deleting a user (admin.md §3.2.1, issue #630). The row stays as a tombstone because messages,
-- reactions, the audit log and call history reference it; `deleted_at` marks it. A deleted user is
-- always deactivated as well, so every check that refuses deactivated accounts refuses it too.
ALTER TABLE users ADD COLUMN deleted_at timestamptz;
ALTER TABLE users
  ADD CONSTRAINT users_deleted_is_deactivated CHECK (deleted_at IS NULL OR deactivated_at IS NOT NULL);

-- Identities a deletion released: the next sign-in with one creates a new account, but tokens
-- from a sign-in before the deletion are refused, so a device of the deleted person can't quietly
-- open that account. One row per deleted identity, kept like the tombstone.
CREATE TABLE released_identities (
  issuer      text        NOT NULL,
  subject     text        NOT NULL,
  released_at timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (issuer, subject)
);

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
