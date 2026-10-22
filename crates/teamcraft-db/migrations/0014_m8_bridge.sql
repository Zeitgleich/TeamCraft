-- Integration channels and bot accounts (ARCHITECTURE.md §4.5).

-- Channels that receive machine output: logs, monitoring, CI. They are end-to-end encrypted like
-- any channel; the flag only changes defaults (retention, no push without a mention).
ALTER TABLE conversations ADD COLUMN is_integration boolean NOT NULL DEFAULT false;

ALTER TABLE users DROP CONSTRAINT users_kind_check;
ALTER TABLE users ADD CONSTRAINT users_kind_check CHECK (kind IN ('member', 'guest', 'bot'));

-- A bot is a user that authenticates with a long random token instead of the identity provider.
-- Only the SHA-256 of the token is stored.
CREATE TABLE bots (
  user_id      bigint PRIMARY KEY REFERENCES users (id) ON DELETE CASCADE,
  name         text        NOT NULL,
  description  text,
  token_hash   bytea       NOT NULL UNIQUE,
  created_by   bigint REFERENCES users (id) ON DELETE SET NULL,
  created_at   timestamptz NOT NULL DEFAULT now(),
  revoked_at   timestamptz
);
