-- The newest MLS group information per conversation (e2ee.md §4.6): a member's new device joins the
-- group from it by external commit. Opaque to the server; only kept for the epoch it was made for.
CREATE TABLE mls_group_infos (
  conversation_id  bigint      PRIMARY KEY REFERENCES conversations (id) ON DELETE CASCADE,
  epoch            bigint      NOT NULL,
  group_info       bytea       NOT NULL,
  updated_at       timestamptz NOT NULL DEFAULT now()
);
