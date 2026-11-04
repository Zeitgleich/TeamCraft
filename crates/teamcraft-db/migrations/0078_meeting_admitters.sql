-- Who may let guests into a meeting besides the organizer's devices (#1103): a statement the
-- organizer's identity key signs (e2ee.md section 4.6, "Meeting guests"). The server keeps the
-- newest per meeting, as bytes, and never verifies it for anybody: guests and members do. Its
-- sequence only grows, so an older statement can't replace a newer one here.
CREATE TABLE meeting_admitters (
  meeting_id  bigint      PRIMARY KEY REFERENCES meetings (id) ON DELETE CASCADE,
  sequence    bigint      NOT NULL,
  statement   bytea       NOT NULL,
  updated_at  timestamptz NOT NULL DEFAULT now()
);
