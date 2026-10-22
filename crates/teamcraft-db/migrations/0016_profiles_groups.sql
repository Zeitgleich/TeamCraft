-- Profiles, avatars and user groups (PLAN.md §4, §11).

CREATE TABLE user_profiles (
  user_id              bigint PRIMARY KEY REFERENCES users (id) ON DELETE CASCADE,
  title                text,
  department           text,
  phone                text,
  -- IANA name; validated by shape, interpreted by clients.
  time_zone            text,
  -- Working hours: bit 0 = Monday ... bit 6 = Sunday, and minutes since local midnight. All three
  -- are NULL or none is.
  working_weekdays     smallint,
  working_start_minute smallint,
  working_end_minute   smallint,
  -- The stored avatar object, set once the upload was verified.
  avatar_key           text,
  avatar_content_type  text,
  avatar_size_bytes    bigint,
  -- An announced upload that has not been completed yet.
  pending_avatar_key          text,
  pending_avatar_content_type text,
  pending_avatar_size_bytes   bigint,
  updated_at           timestamptz NOT NULL DEFAULT now()
);

-- User groups exist since the structure migration; this adds what mentions and the identity provider
-- sync need. Groups are instance wide (space_id stays NULL).
ALTER TABLE user_groups ADD COLUMN handle text;
ALTER TABLE user_groups ADD COLUMN description text;
ALTER TABLE user_groups ADD COLUMN created_by bigint REFERENCES users (id) ON DELETE SET NULL;
UPDATE user_groups SET handle = left(regexp_replace(lower(name), '[^a-z0-9_-]+', '-', 'g'), 24) || '-' || id::text
  WHERE handle IS NULL;
ALTER TABLE user_groups ALTER COLUMN handle SET NOT NULL;
ALTER TABLE user_groups ADD CONSTRAINT user_groups_handle_check CHECK (handle ~ '^[a-z0-9][a-z0-9_-]{1,31}$');
CREATE UNIQUE INDEX user_groups_handle_idx ON user_groups (handle);
-- The identity provider's group ID; set for groups the sync owns.
CREATE UNIQUE INDEX user_groups_external_ref_idx ON user_groups (external_ref) WHERE external_ref IS NOT NULL;
