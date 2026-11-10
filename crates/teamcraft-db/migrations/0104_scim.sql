-- SCIM 2.0 provisioning (issue #1631, docs/provisioning.md §7): identity providers that push
-- instead of being polled create, change, disable and delete accounts and groups through
-- `/scim/v2`, each with a bearer token of its own made in the admin console.

-- A SCIM client: one identity provider application that pushes. The token is
-- `tcscim_<id>_<secret>`; only the SHA-256 of the whole token is kept (it carries 32 random bytes,
-- so no slow hash is needed), compared in constant time after the lookup by ID.
CREATE TABLE scim_clients (
  id                 bigint      PRIMARY KEY,
  name               text        NOT NULL,
  token_sha256       bytea       NOT NULL,
  -- Which SCIM attribute of a user is the subject (`sub`) of their sign-in at the identity
  -- provider: `external_id` or `user_name`. An account made by SCIM is found at its first
  -- sign-in by that subject.
  identity_attribute text        NOT NULL CHECK (identity_attribute IN ('external_id', 'user_name')),
  created_at         timestamptz NOT NULL DEFAULT now(),
  created_by         bigint      REFERENCES users (id) ON DELETE SET NULL,
  token_rotated_at   timestamptz NOT NULL DEFAULT now(),
  last_used_at       timestamptz
);

-- An account as a SCIM resource of one client. The resource's `id` is the account's ID. Removed on
-- a SCIM DELETE (the account itself is deactivated, or deleted if it never signed in).
CREATE TABLE scim_users (
  user_id     bigint      PRIMARY KEY REFERENCES users (id) ON DELETE CASCADE,
  client_id   bigint      NOT NULL REFERENCES scim_clients (id) ON DELETE CASCADE,
  user_name   text        NOT NULL,
  external_id text,
  given_name  text,
  family_name text,
  -- The ETag (`W/"<version>"`): raised by every change through SCIM.
  version     bigint      NOT NULL DEFAULT 1,
  created_at  timestamptz NOT NULL DEFAULT now(),
  updated_at  timestamptz NOT NULL DEFAULT now()
);
-- `userName` is unique per client and compared without case (RFC 7643 §4.1.1).
CREATE UNIQUE INDEX scim_users_user_name ON scim_users (client_id, lower(user_name));
CREATE INDEX scim_users_external_id ON scim_users (client_id, external_id);

-- A group as a SCIM resource. Its members feed the group mapping (docs/provisioning.md §5): a rule
-- whose group is the SCIM group's display name adds its members to the rule's spaces and channels.
CREATE TABLE scim_groups (
  id           bigint      PRIMARY KEY,
  client_id    bigint      NOT NULL REFERENCES scim_clients (id) ON DELETE CASCADE,
  display_name text        NOT NULL,
  external_id  text,
  version      bigint      NOT NULL DEFAULT 1,
  created_at   timestamptz NOT NULL DEFAULT now(),
  updated_at   timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX scim_groups_display_name ON scim_groups (client_id, lower(display_name));
CREATE INDEX scim_groups_external_id ON scim_groups (client_id, external_id);

CREATE TABLE scim_group_members (
  group_id bigint NOT NULL REFERENCES scim_groups (id) ON DELETE CASCADE,
  user_id  bigint NOT NULL REFERENCES users (id) ON DELETE CASCADE,
  PRIMARY KEY (group_id, user_id)
);
CREATE INDEX scim_group_members_user ON scim_group_members (user_id);

-- Deactivations and removals a SCIM client caused, for the safety brake's time window (a SCIM
-- client sends one request per user, not one run). Rows older than the window are pruned.
CREATE TABLE scim_deprovisions (
  user_id bigint      NOT NULL,
  at      timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX scim_deprovisions_at ON scim_deprovisions (at);

-- The safety brake's list is shared by the sync and SCIM: a run of the sync replaces only its own
-- entries, a SCIM hold is applied once an admin confirms it.
ALTER TABLE oidc_sync_held_deactivations
  ADD COLUMN source text NOT NULL DEFAULT 'oidc_sync' CHECK (source IN ('oidc_sync', 'scim'));
