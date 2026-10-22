-- Milestone 7: meetings (PLAN.md §9, ARCHITECTURE.md §6.4).

-- A meeting has its own conversation (the persisted meeting chat), outside spaces.
ALTER TABLE conversations DROP CONSTRAINT conversations_kind_check;
ALTER TABLE conversations
  ADD CONSTRAINT conversations_kind_check CHECK (kind IN ('dm', 'group', 'channel', 'meeting'));

-- Title, time and invitees are server-visible by design: they live in CalDAV and in the invitation
-- e-mails anyway (ARCHITECTURE.md §6.4). The chat and the media are end-to-end encrypted.
CREATE TABLE meetings (
  id                 bigint PRIMARY KEY,
  -- iCalendar UID, also the name of the CalDAV object.
  uid                text        NOT NULL UNIQUE,
  organizer_user_id  bigint      NOT NULL REFERENCES users (id),
  conversation_id    bigint      NOT NULL UNIQUE REFERENCES conversations (id) ON DELETE CASCADE,
  title              text        NOT NULL,
  description        text,
  starts_at          timestamptz NOT NULL,
  ends_at            timestamptz NOT NULL,
  -- RRULE value (without the "RRULE:" prefix) generated from the typed recurrence.
  recurrence_rule    text,
  kind               text        NOT NULL CHECK (kind IN ('scheduled', 'instant')),
  status             text        NOT NULL DEFAULT 'scheduled' CHECK (status IN ('scheduled', 'cancelled', 'ended')),
  -- Guests wait in a lobby until the host admits them.
  has_lobby          boolean     NOT NULL DEFAULT true,
  -- iCalendar SEQUENCE: increased with every update that is sent to invitees.
  sequence           integer     NOT NULL DEFAULT 0,
  created_at         timestamptz NOT NULL DEFAULT now(),
  CONSTRAINT meetings_time_order CHECK (ends_at > starts_at)
);
CREATE INDEX meetings_organizer_idx ON meetings (organizer_user_id, starts_at);
CREATE INDEX meetings_time_idx ON meetings (starts_at);

CREATE TABLE meeting_attendees (
  id            bigint PRIMARY KEY,
  meeting_id    bigint NOT NULL REFERENCES meetings (id) ON DELETE CASCADE,
  -- TeamCraft users; their e-mail may be unknown, then no e-mail invitation is sent.
  user_id       bigint REFERENCES users (id) ON DELETE SET NULL,
  -- Lower-cased; external invitees are identified by it.
  email         text,
  display_name  text,
  rsvp          text   NOT NULL DEFAULT 'needs_action'
    CHECK (rsvp IN ('needs_action', 'accepted', 'declined', 'tentative')),
  CONSTRAINT meeting_attendees_identity CHECK (user_id IS NOT NULL OR email IS NOT NULL)
);
CREATE UNIQUE INDEX meeting_attendees_user_idx ON meeting_attendees (meeting_id, user_id) WHERE user_id IS NOT NULL;
CREATE UNIQUE INDEX meeting_attendees_email_idx ON meeting_attendees (meeting_id, email) WHERE email IS NOT NULL;
CREATE INDEX meeting_attendees_by_user_idx ON meeting_attendees (user_id) WHERE user_id IS NOT NULL;
