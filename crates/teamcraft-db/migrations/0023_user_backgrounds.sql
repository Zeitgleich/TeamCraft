-- A user's own call background pictures. The server only stores where each encrypted picture
-- lives and how large it is; the keys and names are in the user's encrypted key bag.
-- (0022 is taken by the channel moderator role on another branch.)
CREATE TABLE user_backgrounds (
  id           bigint PRIMARY KEY,
  user_id      bigint NOT NULL REFERENCES users (id) ON DELETE CASCADE,
  s3_key       text NOT NULL,
  size_bytes   bigint NOT NULL CHECK (size_bytes > 0),
  created_at   timestamptz NOT NULL DEFAULT now(),
  completed_at timestamptz
);

CREATE INDEX user_backgrounds_user ON user_backgrounds (user_id);
