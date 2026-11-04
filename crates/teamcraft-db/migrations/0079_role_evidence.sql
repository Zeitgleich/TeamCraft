-- Identity provider tokens that name somebody's identity key and roles (e2ee.md section 3.1.3). Owners'
-- apps verify them against the provider and sign the admin roster on them; the server only keeps the
-- newest per person, by the time inside the token, so a retry can't replace it with an older one.
CREATE TABLE role_evidence (
  user_id     bigint      PRIMARY KEY REFERENCES users (id) ON DELETE CASCADE,
  token       text        NOT NULL,
  issued_at   bigint      NOT NULL,
  updated_at  timestamptz NOT NULL DEFAULT now()
);

-- Access tokens with a lower role than the person had, seen on their requests: provider-signed evidence
-- that needs nothing from them, and exists also when they have no role left. Handed out only once expired,
-- so nobody who reads them holds a usable credential.
CREATE TABLE role_losses (
  user_id     bigint      PRIMARY KEY REFERENCES users (id) ON DELETE CASCADE,
  token       text        NOT NULL,
  issued_at   bigint      NOT NULL,
  expires_at  bigint      NOT NULL,
  -- The role the token shows: 'none', or the lower one.
  lost_to     text        NOT NULL,
  updated_at  timestamptz NOT NULL DEFAULT now()
);

-- The role the provider's last token showed. `oidc_role` is also set by the identity provider sync, and a
-- token that shows a lower role has to be noticed also after the sync got there first.
ALTER TABLE users ADD COLUMN oidc_role_seen text;
-- For everybody who exists already: otherwise the column stays empty while their tokens match, and a role
-- the sync lowers then reads as the role last seen, and no drop is noted.
UPDATE users SET oidc_role_seen = oidc_role;
