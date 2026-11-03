-- The picture of a channel, stored like a space's (0054): an object in the blob store (key), the
-- content hash clients cache it by (version) and its size. Directory data like the name, not
-- end-to-end encrypted (e2ee.md §4.4). The three columns are set and cleared together.
ALTER TABLE conversations
  ADD COLUMN image_key        text,
  ADD COLUMN image_version    text,
  ADD COLUMN image_size_bytes bigint,
  ADD CONSTRAINT conversations_image_complete CHECK (
    (image_key IS NULL) = (image_version IS NULL) AND (image_key IS NULL) = (image_size_bytes IS NULL)
  );
