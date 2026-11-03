-- History imported from another chat system (e2ee.md §4.10, admin.md §3.8, issue #402).

-- Imported messages count for no unread or mention count and notify nobody.
ALTER TABLE messages ADD COLUMN is_imported boolean NOT NULL DEFAULT false;

-- The import account while it is a member of a DM or notes without being a participant.
ALTER TABLE conversations ADD COLUMN importer_user_id bigint REFERENCES users (id) ON DELETE SET NULL;
-- The newest imported message (the next one is numbered after it), and the same once the import
-- of the conversation was finished (clients load the history's start again when it changes).
ALTER TABLE conversations ADD COLUMN import_last_message_id bigint;
ALTER TABLE conversations ADD COLUMN imported_through_message_id bigint;
