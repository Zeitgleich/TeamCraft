-- The proof a meeting guest hands the hosts: a MAC over their identity key and name under a secret
-- from the link's fragment, which the server never sees (e2ee.md section 4.6, "Meeting guests").
-- Set once; the server can store it but not forge it.
ALTER TABLE meeting_lobby ADD COLUMN guest_proof bytea;
