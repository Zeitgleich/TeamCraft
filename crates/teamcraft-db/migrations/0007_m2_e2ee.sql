-- Milestone 2: server-side support for end-to-end encryption (docs/e2ee.md).
-- The server stores public material and ciphertext only and makes no trust decisions.

-- OIDC-attested identity key (e2ee.md §3.1): the signed ID token is stored next to the key.
ALTER TABLE identity_keys ADD COLUMN attestation text;
CREATE UNIQUE INDEX identity_keys_active_idx ON identity_keys (user_id) WHERE revoked_at IS NULL;

-- Push registration: provider, token and the shared push key are set together or not at all.
UPDATE devices SET push_token = NULL, push_key = NULL;
ALTER TABLE devices
  ADD COLUMN push_provider text CHECK (push_provider IN ('apns', 'fcm', 'unifiedpush', 'webpush')),
  ADD CONSTRAINT devices_push_complete CHECK (
    (push_provider IS NULL) = (push_token IS NULL) AND (push_token IS NULL) = (push_key IS NULL));

-- At most one unused last-resort key package per device.
CREATE UNIQUE INDEX key_packages_last_resort_idx ON key_packages (device_id)
  WHERE is_last_resort AND consumed_at IS NULL;

-- Current MLS epoch of each conversation. A commit is accepted only for the current epoch and moves
-- it forward in the same transaction: first commit wins. (A unique index on the partitioned
-- mls_messages table would have to include the partition key.)
CREATE TABLE mls_epoch_heads (
  conversation_id  bigint PRIMARY KEY REFERENCES conversations (id) ON DELETE CASCADE,
  epoch            bigint NOT NULL CHECK (epoch >= 0)
);

-- An attachment is downloadable once the upload was confirmed with the declared size.
ALTER TABLE attachments ADD COLUMN completed_at timestamptz;
