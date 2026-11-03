-- Issue #380: each channel's signed role log (e2ee.md §4.6.1, admin.md §2.3). Every entry is a full
-- snapshot of the channel's admins and moderators, signed by the device of somebody allowed to
-- assign them. The server stores and orders the entries; clients verify the signatures and never
-- take the member list's role column for a signed role.
CREATE TABLE channel_role_log (
  conversation_id   bigint      NOT NULL REFERENCES conversations (id) ON DELETE CASCADE,
  version           bigint      NOT NULL CHECK (version >= 1),
  payload           bytea       NOT NULL,
  signature         bytea       NOT NULL,
  signer_device_id  bigint      REFERENCES devices (id) ON DELETE SET NULL,
  created_at        timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (conversation_id, version)
);
