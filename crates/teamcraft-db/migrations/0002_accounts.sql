-- Accounts, devices and key material (ARCHITECTURE.md §4.1, e2ee.md §3).

CREATE TABLE users (
  id            bigint PRIMARY KEY,
  -- Both NULL for local accounts (added later); both set for OIDC accounts.
  oidc_issuer   text,
  oidc_subject  text,
  username      text,
  display_name  text        NOT NULL,
  kind          text        NOT NULL CHECK (kind IN ('member', 'guest')),
  created_at    timestamptz NOT NULL DEFAULT now(),
  deactivated_at timestamptz,
  CONSTRAINT users_oidc_identity CHECK ((oidc_issuer IS NULL) = (oidc_subject IS NULL)),
  CONSTRAINT users_oidc_unique UNIQUE (oidc_issuer, oidc_subject)
);

CREATE TABLE identity_keys (
  id          bigint PRIMARY KEY,
  user_id     bigint      NOT NULL REFERENCES users (id) ON DELETE CASCADE,
  public_key  bytea       NOT NULL,
  -- Signature chain over the key, verified by clients (e2ee.md §3), never trusted by the server.
  signature   bytea       NOT NULL,
  created_at  timestamptz NOT NULL DEFAULT now(),
  revoked_at  timestamptz
);
CREATE INDEX identity_keys_user_idx ON identity_keys (user_id);

CREATE TABLE devices (
  id               bigint PRIMARY KEY,
  user_id          bigint      NOT NULL REFERENCES users (id) ON DELETE CASCADE,
  name             text        NOT NULL,
  public_key       bytea       NOT NULL,
  cross_signature  bytea       NOT NULL,
  push_token       text,
  push_key         bytea,
  created_at       timestamptz NOT NULL DEFAULT now(),
  last_seen_at     timestamptz,
  revoked_at       timestamptz
);
CREATE INDEX devices_user_idx ON devices (user_id);

CREATE TABLE key_packages (
  id               bigint PRIMARY KEY,
  device_id        bigint      NOT NULL REFERENCES devices (id) ON DELETE CASCADE,
  key_package      bytea       NOT NULL,
  is_last_resort   boolean     NOT NULL DEFAULT false,
  created_at       timestamptz NOT NULL DEFAULT now(),
  consumed_at      timestamptz
);
CREATE INDEX key_packages_available_idx ON key_packages (device_id) WHERE consumed_at IS NULL;

CREATE TABLE key_bags (
  user_id             bigint PRIMARY KEY REFERENCES users (id) ON DELETE CASCADE,
  version             bigint      NOT NULL DEFAULT 1,
  encrypted_key_bag   bytea       NOT NULL,
  wrapped_backup_keys bytea       NOT NULL,
  updated_at          timestamptz NOT NULL DEFAULT now()
);
