-- Large attachments go up in parts (S3 multipart, issue #153): the storage's upload ID until the
-- upload is completed or aborted, and when the uploader last asked for a part, so the sweeper only
-- aborts uploads nobody continues.
ALTER TABLE attachments
  ADD COLUMN multipart_upload_id text,
  ADD COLUMN last_activity_at timestamptz;
