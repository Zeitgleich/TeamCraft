-- Profile pictures from the identity provider and uploads through the server (issue #2071).
--
-- A person can have two pictures: their own (avatar_*) and the identity provider's (provider_*).
-- The workspace policy (`directory.avatar_source`) decides which one shows; both are PNGs the
-- server re-encoded itself.
ALTER TABLE user_profiles
  ADD COLUMN provider_avatar_key        text,
  ADD COLUMN provider_avatar_size_bytes bigint,
  -- What the provider's picture was made from: its URL, or `sha256:` and the hash of a SCIM photo
  -- sent inline. A sign-in that names the same source doesn't fetch the picture again.
  ADD COLUMN provider_picture_source    text;

-- Uploads now go to the server, which re-encodes them; announced uploads to object storage are no
-- more. Objects of announced uploads that were never completed go to the cleanup sweep.
INSERT INTO storage_cleanup (s3_key)
  SELECT pending_avatar_key FROM user_profiles WHERE pending_avatar_key IS NOT NULL
  ON CONFLICT (s3_key) DO NOTHING;

ALTER TABLE user_profiles
  DROP COLUMN pending_avatar_key,
  DROP COLUMN pending_avatar_content_type,
  DROP COLUMN pending_avatar_size_bytes;
