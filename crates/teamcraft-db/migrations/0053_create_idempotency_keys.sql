-- Makes creating a space, channel or meeting idempotent: the client-generated key of the request is
-- remembered per user and kind, so a retry after a lost response returns the original resource.
-- Rows are only needed for the retry window and are purged after a week.
CREATE TABLE create_idempotency_keys (
  user_id          bigint      NOT NULL REFERENCES users (id) ON DELETE CASCADE,
  kind             text        NOT NULL,
  idempotency_key  bytea       NOT NULL CHECK (octet_length(idempotency_key) = 16),
  resource_id      bigint      NOT NULL,
  created_at       timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (user_id, kind, idempotency_key)
);

CREATE INDEX create_idempotency_keys_created_idx ON create_idempotency_keys (created_at);
