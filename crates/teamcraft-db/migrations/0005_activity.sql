-- Presence, status, followed threads and the audit log (ARCHITECTURE.md §4.1, §5.3).

CREATE TABLE presence (
  user_id     bigint PRIMARY KEY REFERENCES users (id) ON DELETE CASCADE,
  state       text        NOT NULL CHECK (state IN ('active', 'idle', 'offline')),
  updated_at  timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE custom_status (
  user_id     bigint PRIMARY KEY REFERENCES users (id) ON DELETE CASCADE,
  emoji       text,
  text        text,
  expires_at  timestamptz
);

CREATE TABLE threads_followed (
  user_id          bigint NOT NULL REFERENCES users (id) ON DELETE CASCADE,
  conversation_id  bigint NOT NULL REFERENCES conversations (id) ON DELETE CASCADE,
  thread_id        bigint NOT NULL,
  PRIMARY KEY (user_id, conversation_id, thread_id)
);

-- Signed admin actions and security-relevant events. Contains no message content.
CREATE TABLE audit_log (
  id             bigint PRIMARY KEY,
  actor_user_id  bigint REFERENCES users (id) ON DELETE SET NULL,
  action         text        NOT NULL,
  target_kind    text,
  target_id      bigint,
  detail         jsonb       NOT NULL DEFAULT '{}',
  created_at     timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX audit_log_created_idx ON audit_log (created_at);
