-- The newest signed device list of each user (e2ee.md §3, security-review A4). The server keeps the
-- blob opaque and only enforces that the sequence number never goes down.
CREATE TABLE device_lists (
  user_id     bigint PRIMARY KEY REFERENCES users (id) ON DELETE CASCADE,
  sequence    bigint NOT NULL CHECK (sequence > 0),
  signed_list bytea NOT NULL,
  updated_at  timestamptz NOT NULL DEFAULT now()
);
