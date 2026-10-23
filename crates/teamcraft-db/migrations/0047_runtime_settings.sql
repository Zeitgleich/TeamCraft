-- Settings changed in the admin console (docs/admin.md §6). The configuration file stays the seed:
-- a setting without a row here comes from the file.

-- Mail server, calendar server and the groups of instance settings, one row each. `value` is the
-- setting as JSON without secrets; a password is kept in `sealed_secret`, encrypted with the
-- server's settings key (never stored in plaintext).
CREATE TABLE server_settings (
  name          text        PRIMARY KEY,
  value         jsonb       NOT NULL,
  sealed_secret bytea,
  updated_by    bigint      REFERENCES users (id) ON DELETE SET NULL,
  updated_at    timestamptz NOT NULL DEFAULT now()
);

-- Brands created or taken over in the console (docs/branding.md). A row with the id of a brand of
-- the configuration file replaces that brand.
CREATE TABLE brands (
  id          text        PRIMARY KEY,
  is_default  boolean     NOT NULL DEFAULT false,
  settings    jsonb       NOT NULL,
  updated_by  bigint      REFERENCES users (id) ON DELETE SET NULL,
  updated_at  timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX brands_single_default_idx ON brands (is_default) WHERE is_default;

-- Brand images; the bytes are in the object storage under `object_key`.
CREATE TABLE brand_assets (
  brand_id      text        NOT NULL REFERENCES brands (id) ON DELETE CASCADE,
  slot          text        NOT NULL,
  object_key    text        NOT NULL,
  content_type  text        NOT NULL,
  size_bytes    bigint      NOT NULL,
  updated_at    timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (brand_id, slot)
);
