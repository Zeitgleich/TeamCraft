-- Key packages expire (e2ee.md §4.3). The server records when, hands out only packages that stay
-- valid for a while and deletes the rest. Packages uploaded before this used the openmls default
-- lifetime: 84 days from their creation, right before the upload.
ALTER TABLE key_packages ADD COLUMN expires_at timestamptz;
UPDATE key_packages SET expires_at = created_at + interval '84 days';
ALTER TABLE key_packages ALTER COLUMN expires_at SET NOT NULL;
CREATE INDEX key_packages_expiry_idx ON key_packages (expires_at);
