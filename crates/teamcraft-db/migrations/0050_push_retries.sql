-- Push wake-ups waiting for another attempt after a temporary failure (push service or gateway
-- down, rate limited; notifications.md, "Mobile push delivery"). A worker claims a due row with
-- SELECT ... FOR UPDATE SKIP LOCKED and keeps the row lock while it sends: a crashed server releases
-- the claim with its connection, so the wake-up is neither lost nor sent twice by two workers.
--
-- The row holds the wake-up's fields, never the sealed payload or the device token: the payload is
-- sealed again with the device's current push key when the attempt is made.
CREATE TABLE push_retries (
  id               bigint      PRIMARY KEY,
  user_id          bigint      NOT NULL REFERENCES users (id) ON DELETE CASCADE,
  device_id        bigint      NOT NULL REFERENCES devices (id) ON DELETE CASCADE,
  -- Web Push and UnifiedPush endpoints chosen by users get their own workers, so an endpoint that
  -- never answers can't hold up the retries for APNs and FCM.
  is_user_endpoint boolean     NOT NULL,
  kind             text        NOT NULL,
  conversation_id  bigint      NOT NULL,
  message_id       bigint,
  sender_id        bigint,
  is_urgent        boolean     NOT NULL DEFAULT false,
  thread_id        bigint,
  call_id          bigint,
  badge            integer,
  sent_at_ms       bigint,
  -- Attempts made so far, the first one included.
  attempts         integer     NOT NULL,
  next_attempt_at  timestamptz NOT NULL,
  -- The wake-up's TTL: past it the push service would drop it anyway.
  expires_at       timestamptz NOT NULL,
  created_at       timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX push_retries_due_idx ON push_retries (is_user_endpoint, next_attempt_at);
CREATE INDEX push_retries_device_idx ON push_retries (device_id);
CREATE INDEX push_retries_expires_idx ON push_retries (expires_at);
