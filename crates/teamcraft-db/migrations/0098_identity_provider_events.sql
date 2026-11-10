-- Identity provider events (issue #210, docs/provisioning.md §4): the provider posts "something
-- changed", which starts a sync run soon. One secret per instance, stored as its SHA-256 (32 random
-- bytes need no slow hash). No row: the endpoint is off.
CREATE TABLE identity_provider_event_hook (
  id            boolean     PRIMARY KEY DEFAULT true CHECK (id),
  secret_sha256 bytea       NOT NULL,
  created_at    timestamptz NOT NULL DEFAULT now(),
  created_by    bigint      REFERENCES users (id) ON DELETE SET NULL,
  last_event_at timestamptz
);
