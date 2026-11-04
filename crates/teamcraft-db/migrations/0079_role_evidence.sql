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
  updated_at  timestamptz NOT NULL DEFAULT now()
);
