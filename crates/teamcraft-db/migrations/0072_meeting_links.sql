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
