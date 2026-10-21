-- Message ciphertext and MLS delivery, partitioned by month on the Snowflake ID (see 0001).
-- Partitions are created ahead of time by the server (`Database::ensure_partitions`); the default
-- partition catches anything that slips through so inserts never fail.

CREATE TABLE messages (
  id                bigint NOT NULL,
  conversation_id   bigint NOT NULL REFERENCES conversations (id) ON DELETE CASCADE,
  -- ID of the thread's root message; NULL for top-level messages.
  thread_id         bigint,
  sender_device_id  bigint NOT NULL REFERENCES devices (id),
  content_key_id    bytea  NOT NULL,
  mentioned_user_ids bigint[] NOT NULL DEFAULT '{}',
  -- Ciphertext above the size threshold lives in S3 with only a reference in the row.
  ciphertext        bytea,
  ciphertext_s3_key text,
  CONSTRAINT messages_ciphertext_location CHECK ((ciphertext IS NULL) <> (ciphertext_s3_key IS NULL)),
  PRIMARY KEY (id)
) PARTITION BY RANGE (id);

CREATE TABLE messages_default PARTITION OF messages DEFAULT;
CREATE INDEX messages_conversation_idx ON messages (conversation_id, id);
CREATE INDEX messages_thread_idx ON messages (conversation_id, thread_id, id) WHERE thread_id IS NOT NULL;
SELECT tc_ensure_month_partitions('messages', now() - interval '1 month', 4);

CREATE TABLE mls_messages (
  id                   bigint NOT NULL,
  conversation_id      bigint NOT NULL REFERENCES conversations (id) ON DELETE CASCADE,
  epoch                bigint NOT NULL,
  kind                 text   NOT NULL CHECK (kind IN ('commit', 'proposal', 'welcome', 'application')),
  sender_device_id     bigint NOT NULL REFERENCES devices (id),
  -- Set for welcomes, which are addressed to one new device.
  recipient_device_id  bigint REFERENCES devices (id),
  payload              bytea  NOT NULL,
  PRIMARY KEY (id)
) PARTITION BY RANGE (id);

CREATE TABLE mls_messages_default PARTITION OF mls_messages DEFAULT;
CREATE INDEX mls_messages_conversation_idx ON mls_messages (conversation_id, epoch, id);
CREATE INDEX mls_messages_recipient_idx ON mls_messages (recipient_device_id, id) WHERE recipient_device_id IS NOT NULL;
SELECT tc_ensure_month_partitions('mls_messages', now() - interval '1 month', 4);

CREATE TABLE attachments (
  id               bigint PRIMARY KEY,
  uploader_user_id bigint REFERENCES users (id) ON DELETE SET NULL,
  conversation_id  bigint REFERENCES conversations (id) ON DELETE SET NULL,
  size_bytes       bigint      NOT NULL CHECK (size_bytes >= 0),
  s3_key           text        NOT NULL UNIQUE,
  created_at       timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE index_segments (
  id               bigint PRIMARY KEY,
  conversation_id  bigint      NOT NULL REFERENCES conversations (id) ON DELETE CASCADE,
  range_start      timestamptz NOT NULL,
  range_end        timestamptz NOT NULL,
  s3_key           text        NOT NULL UNIQUE,
  size_bytes       bigint      NOT NULL CHECK (size_bytes >= 0),
  CONSTRAINT index_segments_range CHECK (range_start < range_end)
);
CREATE INDEX index_segments_conversation_idx ON index_segments (conversation_id, range_start);
