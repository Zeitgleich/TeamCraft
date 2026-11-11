-- Issue #1748 (e2ee.md §4.6.5): the creator of a channel whose app founded it with an anchor. The
-- server takes the first entry of such a channel's role log only from them, so nobody else it lets
-- append (a listed channel admin, a workspace admin) takes the version the founding needs. Members
-- never rely on it: they take only the anchor's founding whatever the server stores.
CREATE TABLE anchored_channel_founders (
  conversation_id  bigint PRIMARY KEY REFERENCES conversations (id) ON DELETE CASCADE,
  founder_id       bigint NOT NULL
);
