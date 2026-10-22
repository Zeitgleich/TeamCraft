-- Brands and organizations (docs/branding.md).

-- The organization a user belongs to, a display label synchronized from the identity provider
-- (a configurable token claim). Carries no permission.
ALTER TABLE users ADD COLUMN organization text;
CREATE INDEX users_organization_idx ON users (organization) WHERE organization IS NOT NULL;

-- Optional brand tag of a space: a default theming hint. The id names a configured brand; a brand
-- removed from the configuration simply stops matching.
ALTER TABLE spaces ADD COLUMN brand_id text;

-- The brand that created a share link; the link only works on that brand's host names.
ALTER TABLE share_links ADD COLUMN brand_id text;
