-- Milestone 5: followed threads with read positions (PLAN.md §3).

-- A row exists once a user followed a thread or unfollowed it manually. The 'unfollowed' rows stop
-- automatic follows (replies, mentions) from bringing a thread back; only the user's own reply or
-- an explicit follow does.
ALTER TABLE threads_followed
  ADD COLUMN state                 text        NOT NULL DEFAULT 'following'
    CHECK (state IN ('following', 'unfollowed')),
  ADD COLUMN reason                text        NOT NULL DEFAULT 'manual'
    CHECK (reason IN ('started', 'replied', 'mentioned', 'manual')),
  -- Highest reply the user has read; messages are partitioned, so no foreign key.
  ADD COLUMN last_read_message_id  bigint,
  ADD COLUMN updated_at            timestamptz NOT NULL DEFAULT now();

CREATE INDEX threads_followed_user_idx ON threads_followed (user_id) WHERE state = 'following';

-- Replies are looked up by thread for counts and the inbox.
CREATE INDEX messages_thread_sender_idx ON messages (conversation_id, thread_id, id)
  INCLUDE (sender_user_id) WHERE thread_id IS NOT NULL;

ALTER TABLE users
  -- PLAN.md §3: follow threads where the user is mentioned (user setting).
  ADD COLUMN auto_follow_mentioned_threads boolean NOT NULL DEFAULT true;
