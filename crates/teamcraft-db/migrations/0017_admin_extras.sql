-- Announcement banner, system events of conversations and permission scheme overrides
-- (PLAN.md §13).

-- The instance-wide banner. At most one is active; a new one ends the previous.
CREATE TABLE announcements (
  id             bigint PRIMARY KEY,
  text           text        NOT NULL,
  level          text        NOT NULL CHECK (level IN ('info', 'warning', 'critical')),
  is_dismissible boolean     NOT NULL,
  created_by     bigint REFERENCES users (id) ON DELETE SET NULL,
  created_at     timestamptz NOT NULL DEFAULT now(),
  expires_at     timestamptz,
  ended_at       timestamptz
);
CREATE UNIQUE INDEX announcements_one_active_idx ON announcements ((true)) WHERE ended_at IS NULL;

CREATE TABLE announcement_dismissals (
  announcement_id bigint NOT NULL REFERENCES announcements (id) ON DELETE CASCADE,
  user_id         bigint NOT NULL REFERENCES users (id) ON DELETE CASCADE,
  PRIMARY KEY (announcement_id, user_id)
);

-- What happened to a conversation, written by the server (metadata only): members see it as a
-- system message next to the chat.
CREATE TABLE system_events (
  id               bigint PRIMARY KEY,
  conversation_id  bigint NOT NULL REFERENCES conversations (id) ON DELETE CASCADE,
  kind             text   NOT NULL,
  actor_user_id    bigint REFERENCES users (id) ON DELETE SET NULL,
  subject_user_id  bigint REFERENCES users (id) ON DELETE SET NULL,
  created_at       timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX system_events_conversation_idx ON system_events (conversation_id, id);

-- Changes to the built-in permission scheme. scope_id 0 is the instance, anything else a space.
CREATE TABLE permission_overrides (
  scope_id   bigint NOT NULL,
  role       text   NOT NULL,
  permission text   NOT NULL,
  effect     text   NOT NULL CHECK (effect IN ('grant', 'revoke')),
  PRIMARY KEY (scope_id, role, permission)
);
