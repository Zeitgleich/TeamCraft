-- Issue #714: a client coming back reads the newest MLS entry of each of its conversations in one
-- request (`GET /sync/heads`). `mls_messages_conversation_idx` is ordered by epoch first, so the
-- newest ID needed a scan of the conversation's whole queue; this index makes it one backward probe.
CREATE INDEX mls_messages_conversation_id_idx ON mls_messages (conversation_id, id);
