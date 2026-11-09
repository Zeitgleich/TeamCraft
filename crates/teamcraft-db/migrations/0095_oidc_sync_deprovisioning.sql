-- Dependable deprovisioning (issue #210, ARCHITECTURE.md §5.1.1): how the identity provider sync
-- went, the deactivations its safety brake holds back, and what is left of a deactivation that has
-- been committed.

-- The last run of the sync, one row. Kept in the database so that a restart doesn't blank it.
CREATE TABLE oidc_sync_status (
  id                   boolean     PRIMARY KEY DEFAULT true CHECK (id),
  last_started_at      timestamptz,
  last_finished_at     timestamptz,
  last_success_at      timestamptz,
  last_duration_ms     bigint,
  -- One of `ok`, `partial`, `held`, `dry_run`, `failed`.
  last_outcome         text,
  -- A fixed error code, never the error's text.
  last_error           text,
  consecutive_failures integer     NOT NULL DEFAULT 0,
  deactivated          integer     NOT NULL DEFAULT 0,
  reactivated          integer     NOT NULL DEFAULT 0,
  role_changes         integer     NOT NULL DEFAULT 0,
  groups_changed       integer     NOT NULL DEFAULT 0,
  failed               integer     NOT NULL DEFAULT 0,
  -- Changes whenever the set of held deactivations changes: a confirmation names what it saw.
  held_revision        bigint      NOT NULL DEFAULT 0
);

-- Users the sync would deactivate but the safety brake held back. A confirmation stays on the row
-- until the deactivation happened, so a failure after it needs no second confirmation.
CREATE TABLE oidc_sync_held_deactivations (
  user_id      bigint      PRIMARY KEY REFERENCES users (id) ON DELETE CASCADE,
  -- One of `removed`, `disabled`, `no_role`.
  cause        text        NOT NULL,
  held_at      timestamptz NOT NULL DEFAULT now(),
  confirmed_at timestamptz,
  confirmed_by bigint      REFERENCES users (id) ON DELETE SET NULL
);

-- A deactivation committed but not finished: sessions, calls and announcements still to be done.
-- Written with the deactivation, deleted when the steps ran; a crash in between is finished later.
CREATE TABLE user_deactivation_followups (
  user_id         bigint      PRIMARY KEY REFERENCES users (id) ON DELETE CASCADE,
  created_at      timestamptz NOT NULL DEFAULT now(),
  attempts        integer     NOT NULL DEFAULT 0,
  last_attempt_at timestamptz
);

-- A bot is paused while the person who created it (and saw its token) is deactivated.
ALTER TABLE users DROP CONSTRAINT users_deactivation_reason_check;
ALTER TABLE users
  ADD CONSTRAINT users_deactivation_reason_check
  CHECK (deactivation_reason IN ('admin', 'oidc', 'left', 'creator_deactivated'));
