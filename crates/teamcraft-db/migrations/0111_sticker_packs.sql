-- Sticker packs of the workspace (issue #2132, docs/e2ee.md §4.14): like custom emoji, still PNG or
-- WebP pictures in object storage, unencrypted and keyed by their SHA-256, and per pack the document
-- an admin's device signed over its name and stickers. The server checks the pictures and the
-- document's rules; clients verify the signature against the signed admin roster before using a pack.
CREATE TABLE sticker_pictures (
  id           bigint      PRIMARY KEY,
  sha256       bytea       NOT NULL UNIQUE CHECK (length(sha256) = 32),
  -- `image/png` or `image/webp`.
  format       text        NOT NULL,
  s3_key       text        NOT NULL,
  size_bytes   bigint      NOT NULL CHECK (size_bytes > 0),
  width        integer     NOT NULL CHECK (width > 0),
  height       integer     NOT NULL CHECK (height > 0),
  uploaded_by  bigint      REFERENCES users (id) ON DELETE SET NULL,
  uploaded_at  timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE sticker_packs (
  id          bigint      PRIMARY KEY,
  -- Chosen at random by the device that created the pack; the signed document names it.
  pack_key    bytea       NOT NULL UNIQUE CHECK (length(pack_key) = 16),
  version     bigint      NOT NULL CHECK (version > 0),
  name        text        NOT NULL,
  -- The signed document (JSON) and its signature, verbatim.
  payload     bytea       NOT NULL,
  signature   bytea       NOT NULL,
  updated_by  bigint      REFERENCES users (id) ON DELETE SET NULL,
  updated_at  timestamptz NOT NULL DEFAULT now()
);

-- The pictures the current version of each pack names: a picture no pack names is deleted with its
-- object once it is older than an upload in progress could be.
CREATE TABLE sticker_pack_pictures (
  pack_id     bigint NOT NULL REFERENCES sticker_packs (id) ON DELETE CASCADE,
  picture_id  bigint NOT NULL REFERENCES sticker_pictures (id),
  PRIMARY KEY (pack_id, picture_id)
);

CREATE INDEX sticker_pack_pictures_picture ON sticker_pack_pictures (picture_id);
