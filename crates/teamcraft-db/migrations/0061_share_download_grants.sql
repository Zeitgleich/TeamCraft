-- A download is counted when the server hands out the ciphertext URL, but a transient failure on
-- the way (the storage fetch, the connection) must not burn a link with a download limit. The
-- server therefore returns a grant with the count; presenting it again within a short window gets a
-- fresh URL without counting. Only the hash of the grant is stored, one grant per link (a second
-- downloader replaces it, whose retry then counts again: it fails safe).

ALTER TABLE share_links
  ADD COLUMN grant_sha256 bytea,
  ADD COLUMN grant_until  timestamptz;
