-- Which reactions to their own messages a member saw (notifications.md, "Reactions to my
-- messages", issue #1009): every message up to `reactions_seen_through_message_id`, and the
-- reaction messages after it in `reactions_seen_message_ids` (ascending, bounded by
-- `limits.max_seen_reactions`). Reactions are encrypted, so these are plain message IDs.
ALTER TABLE memberships
  ADD COLUMN reactions_seen_through_message_id bigint,
  ADD COLUMN reactions_seen_message_ids bigint[] NOT NULL DEFAULT '{}';

-- Reactions from before this version that were read count as seen: nobody is greeted by a button
-- for everything that ever happened.
UPDATE memberships SET reactions_seen_through_message_id = last_read_message_id;
