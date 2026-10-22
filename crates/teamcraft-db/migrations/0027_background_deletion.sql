-- Deleted backgrounds keep their row (and count towards the limits) until their presigned upload
-- URLs have expired; a sweeper then deletes the object once more and the row. Uploads announced
-- but never completed are swept the same way.
ALTER TABLE user_backgrounds ADD COLUMN deleted_at timestamptz;
ALTER TABLE org_backgrounds ADD COLUMN deleted_at timestamptz;
CREATE INDEX user_backgrounds_sweep ON user_backgrounds (deleted_at, completed_at, created_at);
CREATE INDEX org_backgrounds_sweep ON org_backgrounds (deleted_at, completed_at, created_at);
