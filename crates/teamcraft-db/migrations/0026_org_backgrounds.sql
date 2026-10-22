-- Call background pictures admins uploaded for the organization (admin.md §3.6). Stored
-- unencrypted; the owner-signed workspace policy lists them with their SHA-256.
CREATE TABLE org_backgrounds (
  id           bigint PRIMARY KEY,
  uploaded_by  bigint REFERENCES users (id) ON DELETE SET NULL,
  s3_key       text NOT NULL,
  size_bytes   bigint NOT NULL CHECK (size_bytes > 0),
  created_at   timestamptz NOT NULL DEFAULT now(),
  completed_at timestamptz
);
