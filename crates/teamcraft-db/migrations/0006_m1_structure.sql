-- Milestone 1: categories, invites, presence preferences, message routing metadata.

ALTER TABLE users
  ADD COLUMN email          text,
  ADD COLUMN presence_pref  text        NOT NULL DEFAULT 'auto' CHECK (presence_pref IN ('auto', 'dnd', 'invisible')),
  ADD COLUMN dnd_until      timestamptz;
CREATE INDEX users_email_idx ON users (lower(email)) WHERE email IS NOT NULL;

ALTER TABLE spaces
  ADD COLUMN color        text,
  ADD COLUMN description  text;

CREATE TABLE channel_categories (
  id          bigint PRIMARY KEY,
  space_id    bigint      NOT NULL REFERENCES spaces (id) ON DELETE CASCADE,
  name        text        NOT NULL,
  position    integer     NOT NULL DEFAULT 0,
  created_at  timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX channel_categories_space_idx ON channel_categories (space_id, position);

-- Guests invited to a space have an expiry and a responsible employee (PLAN.md §12).
ALTER TABLE space_members
  ADD COLUMN expires_at       timestamptz,
  ADD COLUMN sponsor_user_id  bigint REFERENCES users (id) ON DELETE SET NULL;

ALTER TABLE conversations
  DROP COLUMN category,
  ADD COLUMN category_id         bigint REFERENCES channel_categories (id) ON DELETE SET NULL,
  ADD COLUMN topic               text,
  ADD COLUMN description         text,
  -- Channel moderation: permissions removed from members / guests (never added).
  ADD COLUMN member_denied       text[] NOT NULL DEFAULT '{}',
  ADD COLUMN guest_denied        text[] NOT NULL DEFAULT '{}',
  -- Sorted participant IDs of a DM / group DM, so the same people always get the same conversation.
  ADD COLUMN participants_key    text,
  ADD CONSTRAINT conversations_participants_key CHECK ((kind IN ('dm', 'group')) = (participants_key IS NOT NULL));
CREATE UNIQUE INDEX conversations_participants_key_idx ON conversations (participants_key)
  WHERE participants_key IS NOT NULL;

ALTER TABLE memberships ADD COLUMN is_favorite boolean NOT NULL DEFAULT false;

CREATE TABLE invites (
  id               bigint PRIMARY KEY,
  space_id         bigint      NOT NULL REFERENCES spaces (id) ON DELETE CASCADE,
  email            text        NOT NULL,
  space_role       text        NOT NULL CHECK (space_role IN ('space_admin', 'member', 'guest')),
  channel_ids      bigint[]    NOT NULL DEFAULT '{}',
  sponsor_user_id  bigint      NOT NULL REFERENCES users (id) ON DELETE CASCADE,
  expires_at       timestamptz,
  created_at       timestamptz NOT NULL DEFAULT now(),
  accepted_at      timestamptz,
  accepted_by      bigint REFERENCES users (id) ON DELETE SET NULL
);
CREATE INDEX invites_pending_email_idx ON invites (lower(email)) WHERE accepted_at IS NULL;
CREATE INDEX invites_space_idx ON invites (space_id);

ALTER TABLE messages
  ADD COLUMN client_message_id  bytea   NOT NULL CHECK (octet_length(client_message_id) = 16),
  ADD COLUMN sender_user_id     bigint  NOT NULL REFERENCES users (id),
  ADD COLUMN is_urgent          boolean NOT NULL DEFAULT false;

-- Makes sends idempotent. A unique index on a partitioned table must contain the partition key, so
-- the client ID is deduplicated in this small side table instead.
CREATE TABLE message_client_ids (
  conversation_id    bigint NOT NULL REFERENCES conversations (id) ON DELETE CASCADE,
  client_message_id  bytea  NOT NULL CHECK (octet_length(client_message_id) = 16),
  message_id         bigint NOT NULL,
  PRIMARY KEY (conversation_id, client_message_id)
);
