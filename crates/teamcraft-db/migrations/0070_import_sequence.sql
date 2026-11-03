-- Imported messages share one ID sequence across all conversations (admin.md §3.8): its end is
-- found through this index, not by scanning every message.
CREATE INDEX messages_imported_idx ON messages (id) WHERE is_imported;
