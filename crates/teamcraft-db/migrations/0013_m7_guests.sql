-- Milestone 7: one-time guest invitations and the lobby of a meeting (PLAN.md §9, §12).

-- A personal join link for somebody without an account. The link carries a signed token naming the
-- invite; whether it is still good is decided here (not redeemed yet, meeting not over).
CREATE TABLE meeting_invites (
  id              bigint PRIMARY KEY,
  meeting_id      bigint      NOT NULL REFERENCES meetings (id) ON DELETE CASCADE,
  -- Set for invitations sent by e-mail.
  email           text,
  -- Free text for links the host handed out by hand.
  label           text,
  created_by      bigint REFERENCES users (id) ON DELETE SET NULL,
  created_at      timestamptz NOT NULL DEFAULT now(),
  redeemed_at     timestamptz,
  -- The guest account that redeemed the invite.
  guest_user_id   bigint REFERENCES users (id) ON DELETE SET NULL
);
CREATE UNIQUE INDEX meeting_invites_open_email_idx ON meeting_invites (meeting_id, email)
  WHERE email IS NOT NULL AND redeemed_at IS NULL;
CREATE INDEX meeting_invites_meeting_idx ON meeting_invites (meeting_id);

-- Guests wait here until a host admits them.
CREATE TABLE meeting_lobby (
  meeting_id    bigint      NOT NULL REFERENCES meetings (id) ON DELETE CASCADE,
  user_id       bigint      NOT NULL REFERENCES users (id) ON DELETE CASCADE,
  state         text        NOT NULL DEFAULT 'waiting' CHECK (state IN ('waiting', 'admitted', 'denied')),
  requested_at  timestamptz NOT NULL DEFAULT now(),
  decided_at    timestamptz,
  decided_by    bigint REFERENCES users (id) ON DELETE SET NULL,
  PRIMARY KEY (meeting_id, user_id)
);
