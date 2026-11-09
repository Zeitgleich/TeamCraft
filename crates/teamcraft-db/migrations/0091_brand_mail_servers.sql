-- A brand's own mail server (docs/admin.md §6.2). A brand without a row sends through the instance's
-- mail server. `value` is the server as JSON without secrets; a password is kept in `sealed_secret`,
-- encrypted with the server's settings key (never stored in plaintext). Only brands of the database
-- can have one, so it goes when the brand goes.
CREATE TABLE brand_mail_servers (
  brand_id      text        PRIMARY KEY REFERENCES brands (id) ON DELETE CASCADE,
  value         jsonb       NOT NULL,
  sealed_secret bytea,
  updated_by    bigint      REFERENCES users (id) ON DELETE SET NULL,
  updated_at    timestamptz NOT NULL DEFAULT now()
);
