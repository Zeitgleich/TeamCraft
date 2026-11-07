-- Files of an unsent draft (#1220) go up before the message: held until `draft_until`, when the
-- sweeper removes them unless the uploader sent them (the column is cleared then). Only the uploader
-- may download a held file; nobody else has its key anyway.
ALTER TABLE attachments ADD COLUMN draft_until timestamptz;

CREATE INDEX attachments_draft_until_idx ON attachments (draft_until) WHERE draft_until IS NOT NULL;
