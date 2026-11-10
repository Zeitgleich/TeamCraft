-- Custom emoji of the workspace (issue #207, docs/e2ee.md §4.12): a still PNG or WebP picture in
-- object storage, unencrypted like the organization's call backgrounds, and the document an admin's
-- device signed over its name and hash. The server checks the picture and the document's name and
-- hash; clients verify the signature against the signed admin roster before showing a picture.
CREATE TABLE custom_emoji (
  id          bigint      PRIMARY KEY,
  shortcode   text        NOT NULL,
  sha256      bytea       NOT NULL CHECK (length(sha256) = 32),
  -- `image/png` or `image/webp`.
  format      text        NOT NULL,
  s3_key      text        NOT NULL,
  size_bytes  bigint      NOT NULL CHECK (size_bytes > 0),
  width       integer     NOT NULL CHECK (width > 0),
  height      integer     NOT NULL CHECK (height > 0),
  -- The signed document (JSON) and its signature, verbatim.
  payload     bytea       NOT NULL,
  signature   bytea       NOT NULL,
  created_by  bigint      REFERENCES users (id) ON DELETE SET NULL,
  created_at  timestamptz NOT NULL DEFAULT now()
);

-- One emoji per name. A deleted emoji's row goes at once (its object with it), freeing the name.
CREATE UNIQUE INDEX custom_emoji_shortcode ON custom_emoji (shortcode);
