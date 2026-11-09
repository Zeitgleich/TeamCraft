-- Delivered ticks (issue #598). The highest message ID that a device of the member stored, moved
-- forward by the member's devices after they store messages (e2ee.md §4.4, notifications.md "Read
-- receipts"). One value per membership, so it doesn't grow with the history. Messages are
-- partitioned, so no foreign key.
ALTER TABLE memberships ADD COLUMN last_delivered_message_id BIGINT;

-- Whether the user shares delivery with the other members of DMs and group DMs; NULL: the
-- instance default for receipts applies. Reciprocal, like `send_read_receipts`.
ALTER TABLE user_notification_settings ADD COLUMN send_delivery_receipts BOOLEAN;
