-- When an invitation mail last went out, so resending can be limited (issue #298).
ALTER TABLE invites ADD COLUMN last_sent_at timestamptz NOT NULL DEFAULT now();

-- One open invitation per address and space. Of repeated ones the one that still works is kept: no
-- expiry first, then the latest expiry, then the newest. Expired ones without a sibling stay, so they
-- can still be seen and revoked.
DELETE FROM invites WHERE id IN (
  SELECT ranked.id FROM (
    SELECT id, row_number() OVER (
      PARTITION BY space_id, lower(email)
      ORDER BY expires_at DESC NULLS FIRST, id DESC
    ) AS position
    FROM invites WHERE accepted_at IS NULL
  ) ranked
  WHERE ranked.position > 1
);
CREATE UNIQUE INDEX invites_open_address_idx ON invites (space_id, lower(email)) WHERE accepted_at IS NULL;

-- The directory's pages walk people and channels by name (issue #298). At 100k users the first page
-- of people goes from about 70 ms to under 1 ms; at 30k channels the first page from 30 to 6 ms.
CREATE INDEX users_directory_order_idx ON users (display_name, id);
CREATE INDEX conversations_directory_order_idx ON conversations (name, id) WHERE name IS NOT NULL;
