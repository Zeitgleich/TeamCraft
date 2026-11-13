-- Tokens of members with no role, kept so that an owner's app notes which identity provider account is
-- theirs (e2ee.md section 3.1, issue #1378). They are many more than the role holders' tokens and must
-- never crowd those out, so the owners' apps read them separately, a page at a time.
ALTER TABLE role_evidence ADD COLUMN for_account boolean NOT NULL DEFAULT false;

CREATE INDEX role_evidence_for_account ON role_evidence (for_account, updated_at);
