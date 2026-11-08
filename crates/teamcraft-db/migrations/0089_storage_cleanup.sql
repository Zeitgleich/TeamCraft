-- Objects the server could not delete from the object storage (issue #743). Their rows are already
-- gone, so nothing else remembers them; the background sweep deletes them once the storage answers.
CREATE TABLE storage_cleanup (
    s3_key TEXT PRIMARY KEY,
    -- Set for an unfinished upload in parts, which is aborted instead of deleted.
    multipart_upload_id TEXT,
    queued_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX storage_cleanup_queued_at_idx ON storage_cleanup (queued_at);
