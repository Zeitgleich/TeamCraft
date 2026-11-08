-- Per-conversation notification filters (notifications.md, "Per-conversation filters", issue #599):
-- whether @channel/@here/@all count as a mention of the member here, and quiet hours during which
-- the conversation acts muted. Both are the member's own and independent of the space inheritance.

ALTER TABLE memberships
  ADD COLUMN channel_mentions text NOT NULL DEFAULT 'default',
  -- JSON text the database doesn't interpret (`QuietHours`), validated by the server on write.
  ADD COLUMN quiet_hours text;

ALTER TABLE scheduled_messages
  -- What `is_channel_mention` of an immediate send says, kept until the message is released.
  ADD COLUMN is_channel_mention boolean NOT NULL DEFAULT false;
