-- Meeting links (#987): an unguessable token per meeting replaces its snowflake ID as the public
-- handle (`<base>/meeting/<token>`). The server makes it on first need and replaces it when the
-- organizer makes the link anew; the old token then names nothing. It carries no key (e2ee.md
-- section 4.6, "Meeting guests").
CREATE TABLE meeting_links (
  meeting_id  bigint      PRIMARY KEY REFERENCES meetings (id) ON DELETE CASCADE,
  token       text        NOT NULL UNIQUE,
  created_at  timestamptz NOT NULL DEFAULT now()
);

-- Who knocked with the meeting link's token rather than an old link: in a meeting without a lobby
-- the organizer's device lets them in by itself. Guests of old links always wait for a click.
ALTER TABLE meeting_lobby ADD COLUMN is_by_link boolean NOT NULL DEFAULT false;

-- The identity key fingerprint a waiting guest's page reported with its device (#987 review): the
-- organizer's device adds the guest only if the key it pins has this fingerprint, and the hosts are
-- told about the device once (`device_announced_at`).
ALTER TABLE meeting_lobby ADD COLUMN knock_fingerprint text;
ALTER TABLE meeting_lobby ADD COLUMN device_announced_at timestamptz;

-- Users whose app lets guests in by the flow of #987 (no link secret): without one, a guest's page
-- says the organizer's app needs an update instead of waiting forever.
CREATE TABLE guest_admitters (
  user_id  bigint      PRIMARY KEY REFERENCES users (id) ON DELETE CASCADE,
  seen_at  timestamptz NOT NULL DEFAULT now()
);
