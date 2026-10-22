-- Share links for external download (e2ee.md §4.5.1). The file key stays in the URL fragment and
-- never reaches the server; the server only knows which blob the link serves and the limits.

CREATE TABLE share_links (
  -- Random and unguessable (256 bits, URL-safe), not a Snowflake.
  id                  text PRIMARY KEY,
  attachment_id       bigint NOT NULL REFERENCES attachments (id) ON DELETE CASCADE,
  created_by          bigint NOT NULL REFERENCES users (id) ON DELETE CASCADE,
  created_at          timestamptz NOT NULL DEFAULT now(),
  expires_at          timestamptz NOT NULL,
  max_downloads       integer CHECK (max_downloads IS NULL OR max_downloads > 0),
  -- The key is wrapped with a password on top; the download page asks for it. Informational for
  -- the server, which never sees the password.
  is_password_protected boolean NOT NULL DEFAULT false,
  download_count      integer NOT NULL DEFAULT 0,
  last_downloaded_at  timestamptz,
  revoked_at          timestamptz
);
CREATE INDEX share_links_creator_idx ON share_links (created_by, created_at DESC);
CREATE INDEX share_links_attachment_idx ON share_links (attachment_id);
