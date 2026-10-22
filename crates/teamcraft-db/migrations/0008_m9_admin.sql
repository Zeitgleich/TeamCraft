-- Milestone 9: signed workspace log, admin features, retention, OIDC deprovisioning.

-- Append-only logs signed by workspace owners (admin.md §2.3, e2ee.md §11): the admin roster and the
-- workspace policy. The server stores and orders them; clients verify the signatures.
CREATE TABLE workspace_log (
  kind              text        NOT NULL CHECK (kind IN ('roster', 'policy')),
  version           bigint      NOT NULL CHECK (version >= 1),
  payload           bytea       NOT NULL,
  signature         bytea       NOT NULL,
  signer_device_id  bigint REFERENCES devices (id),
  created_at        timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (kind, version)
);

-- Why and how a user was deactivated; only OIDC-caused deactivations are undone by the OIDC sync.
ALTER TABLE users
  ADD COLUMN deactivation_reason text CHECK (deactivation_reason IN ('admin', 'oidc')),
  ADD COLUMN oidc_role text;

-- Retention (PLAN.md §1): keep the last N days and/or the last N messages. NULL inherits.
ALTER TABLE spaces
  ADD COLUMN retention_max_age_days  integer CHECK (retention_max_age_days > 0),
  ADD COLUMN retention_max_messages  bigint  CHECK (retention_max_messages > 0);
ALTER TABLE conversations
  ADD COLUMN retention_max_age_days  integer CHECK (retention_max_age_days > 0),
  ADD COLUMN retention_max_messages  bigint  CHECK (retention_max_messages > 0);
ALTER TABLE messages ADD COLUMN is_pinned boolean NOT NULL DEFAULT false;

CREATE INDEX audit_log_actor_idx ON audit_log (actor_user_id, id);
CREATE INDEX audit_log_target_idx ON audit_log (target_kind, target_id, id);
CREATE INDEX audit_log_action_idx ON audit_log (action, id);
