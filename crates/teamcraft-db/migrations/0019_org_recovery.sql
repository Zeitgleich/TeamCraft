-- Organization recovery (e2ee.md §6.3.1): the server stores sealed material and counts approvals;
-- it never sees a share, the org key or a backup key.

-- Shares of the org secret, each sealed to one device of one admin. A new key version replaces all.
CREATE TABLE org_recovery_shares (
  device_id    bigint PRIMARY KEY REFERENCES devices (id) ON DELETE CASCADE,
  admin_id     bigint NOT NULL REFERENCES users (id) ON DELETE CASCADE,
  key_version  bigint NOT NULL,
  sealed       bytea  NOT NULL,
  created_at   timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX org_recovery_shares_admin_idx ON org_recovery_shares (admin_id);

CREATE TABLE org_recovery_requests (
  id                  bigint PRIMARY KEY,
  user_id             bigint NOT NULL REFERENCES users (id) ON DELETE CASCADE,
  request_public_key  bytea  NOT NULL,
  fingerprint         text   NOT NULL,
  status              text   NOT NULL DEFAULT 'pending'
    CHECK (status IN ('pending', 'approved', 'completed', 'denied', 'cancelled', 'expired')),
  required_approvals  integer NOT NULL,
  created_at          timestamptz NOT NULL DEFAULT now(),
  expires_at          timestamptz NOT NULL,
  -- The admin whose approval reached the threshold; their device combines the shares.
  combining_admin_id  bigint REFERENCES users (id) ON DELETE SET NULL,
  combining_device_id bigint REFERENCES devices (id) ON DELETE SET NULL,
  -- The backup key sealed to the request key, relayed to the recovering device.
  result_sealed       bytea,
  completed_at        timestamptz
);
-- One open request per person.
CREATE UNIQUE INDEX org_recovery_open_request_idx ON org_recovery_requests (user_id)
  WHERE status IN ('pending', 'approved');

CREATE TABLE org_recovery_approvals (
  request_id  bigint NOT NULL REFERENCES org_recovery_requests (id) ON DELETE CASCADE,
  admin_id    bigint NOT NULL REFERENCES users (id) ON DELETE CASCADE,
  device_id   bigint NOT NULL REFERENCES devices (id) ON DELETE CASCADE,
  approved_at timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (request_id, admin_id)
);

-- Shares the other approvers sealed to the combining admin's device.
CREATE TABLE org_recovery_request_shares (
  request_id    bigint NOT NULL REFERENCES org_recovery_requests (id) ON DELETE CASCADE,
  from_admin_id bigint NOT NULL REFERENCES users (id) ON DELETE CASCADE,
  sealed        bytea  NOT NULL,
  PRIMARY KEY (request_id, from_admin_id)
);
