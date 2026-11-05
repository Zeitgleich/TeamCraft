-- A meeting can belong to a space (#954): the one it was scheduled in. Its chat stays outside spaces
-- (`conversations_space_by_kind`); this only decides where the meeting is listed. A meeting from
-- before this change, or of a deleted space, belongs to none and is listed outside spaces only.
ALTER TABLE meetings ADD COLUMN space_id bigint REFERENCES spaces (id) ON DELETE SET NULL;

CREATE INDEX meetings_space_idx ON meetings (space_id) WHERE space_id IS NOT NULL;
