-- Co-hosts and handing a meeting over (#1720): the organizer's identity key signs who hosts the
-- meeting (e2ee.md section 4.6, "Meeting hosts"). The server keeps the lists as bytes and never
-- verifies them for anybody: guests and members do. It keeps every handover (a list naming another
-- organizer), so a link made with an older organizer's key still leads to the newest list, and of
-- the other lists only the newest. Sequences only grow.
CREATE TABLE meeting_host_lists (
  meeting_id   bigint      NOT NULL REFERENCES meetings (id) ON DELETE CASCADE,
  sequence     bigint      NOT NULL,
  statement    bytea       NOT NULL,
  is_handover  boolean     NOT NULL,
  created_at   timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (meeting_id, sequence)
);

-- The co-hosts the newest list names, as the server reads them: who may admit, deny and make guest
-- links besides the organizer. Replaced with every list.
CREATE TABLE meeting_co_hosts (
  meeting_id  bigint      NOT NULL REFERENCES meetings (id) ON DELETE CASCADE,
  user_id     bigint      NOT NULL REFERENCES users (id) ON DELETE CASCADE,
  added_at    timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (meeting_id, user_id)
);
CREATE INDEX meeting_co_hosts_user_idx ON meeting_co_hosts (user_id);
