-- Local guest accounts (PLAN.md §12) and server-side session tracking (PLAN.md §11).

-- Credentials of guests that do not come from the identity provider. The user row itself lives in
-- `users` under the server's own issuer.
CREATE TABLE local_accounts (
  user_id               bigint PRIMARY KEY REFERENCES users (id) ON DELETE CASCADE,
  -- Lower-case e-mail address the guest signs in with.
  login                 text        NOT NULL UNIQUE,
  -- NULL until the guest followed the activation link and chose a password.
  password_hash         text,
  -- SHA-256 of the one-time activation token, cleared when it was used.
  activation_digest     bytea,
  activation_expires_at timestamptz,
  -- TOTP secret sealed with the server's account key; confirmed by the first valid code.
  totp_secret_sealed    bytea,
  is_totp_confirmed     boolean     NOT NULL DEFAULT false,
  -- Latest accepted TOTP time step; a code is not accepted twice.
  totp_last_step        bigint,
  failed_attempts       integer     NOT NULL DEFAULT 0,
  locked_until          timestamptz,
  created_at            timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX local_accounts_activation_idx ON local_accounts (activation_digest)
  WHERE activation_digest IS NOT NULL;

-- Sessions as the server sees them, for session length and idle timeout. For the identity provider
-- the key is the `sid` claim; local sessions get their own key at sign-in.
CREATE TABLE session_activity (
  issuer       text        NOT NULL,
  subject      text        NOT NULL,
  session_key  text        NOT NULL,
  started_at   timestamptz NOT NULL DEFAULT now(),
  last_seen_at timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (issuer, subject, session_key)
);
CREATE INDEX session_activity_seen_idx ON session_activity (last_seen_at);
