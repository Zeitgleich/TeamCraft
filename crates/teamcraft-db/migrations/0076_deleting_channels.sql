-- Deleting channels and spaces (issue #1130): the request only marks them, removes the memberships and
-- tells the devices; a background job clears storage and rows, so a big channel can't outlast a proxy.
-- Every query that lists them looks past a marked row.
ALTER TABLE conversations ADD COLUMN deleted_at timestamptz;
ALTER TABLE spaces ADD COLUMN deleted_at timestamptz;
CREATE INDEX conversations_deleted_idx ON conversations (id) WHERE deleted_at IS NOT NULL;
CREATE INDEX spaces_deleted_idx ON spaces (id) WHERE deleted_at IS NOT NULL;
