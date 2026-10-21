-- Spaces, conversations and memberships (ARCHITECTURE.md §4.1, §5.2).

CREATE TABLE spaces (
  id           bigint PRIMARY KEY,
  name         text        NOT NULL,
  icon_ref     text,
  join_policy  text        NOT NULL DEFAULT 'invite_only' CHECK (join_policy IN ('open', 'invite_only')),
  created_at   timestamptz NOT NULL DEFAULT now(),
  archived_at  timestamptz
);

CREATE TABLE space_members (
  space_id   bigint      NOT NULL REFERENCES spaces (id) ON DELETE CASCADE,
  user_id    bigint      NOT NULL REFERENCES users (id) ON DELETE CASCADE,
  role       text        NOT NULL CHECK (role IN ('space_admin', 'member', 'guest')),
  joined_at  timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (space_id, user_id)
);
CREATE INDEX space_members_user_idx ON space_members (user_id);

CREATE TABLE conversations (
  id              bigint PRIMARY KEY,
  space_id        bigint REFERENCES spaces (id) ON DELETE CASCADE,
  kind            text        NOT NULL CHECK (kind IN ('dm', 'group', 'channel')),
  name            text,
  category        text,
  visibility      text        NOT NULL DEFAULT 'private' CHECK (visibility IN ('public', 'private')),
  history_policy  text        NOT NULL DEFAULT 'from_join' CHECK (history_policy IN ('from_join', 'full')),
  created_at      timestamptz NOT NULL DEFAULT now(),
  archived_at     timestamptz,
  -- DMs and groups live outside spaces; channels always belong to one.
  CONSTRAINT conversations_space_by_kind CHECK ((kind = 'channel') = (space_id IS NOT NULL)),
  CONSTRAINT conversations_dm_private CHECK (kind <> 'dm' OR visibility = 'private')
);
CREATE INDEX conversations_space_idx ON conversations (space_id) WHERE space_id IS NOT NULL;

CREATE TABLE memberships (
  conversation_id       bigint      NOT NULL REFERENCES conversations (id) ON DELETE CASCADE,
  user_id               bigint      NOT NULL REFERENCES users (id) ON DELETE CASCADE,
  role                  text        NOT NULL DEFAULT 'member' CHECK (role IN ('admin', 'member')),
  notification_level    text        NOT NULL DEFAULT 'mentions' CHECK (notification_level IN ('all', 'mentions', 'none')),
  -- Highest message ID the user has read; messages are partitioned, so no foreign key.
  last_read_message_id  bigint,
  joined_at             timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (conversation_id, user_id)
);
CREATE INDEX memberships_user_idx ON memberships (user_id);

CREATE TABLE user_groups (
  id           bigint PRIMARY KEY,
  space_id     bigint REFERENCES spaces (id) ON DELETE CASCADE,
  name         text        NOT NULL,
  -- Path of the matching identity provider group, e.g. '/project-a', for automatic membership.
  external_ref text,
  created_at   timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE user_group_members (
  group_id  bigint NOT NULL REFERENCES user_groups (id) ON DELETE CASCADE,
  user_id   bigint NOT NULL REFERENCES users (id) ON DELETE CASCADE,
  PRIMARY KEY (group_id, user_id)
);
CREATE INDEX user_group_members_user_idx ON user_group_members (user_id);
