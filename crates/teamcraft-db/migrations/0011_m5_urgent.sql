-- Milestone 5: acknowledgements of urgent messages and the repeat schedule of urgent direct
-- messages (PLAN.md §5).

CREATE TABLE message_acknowledgements (
  message_id       bigint      NOT NULL,
  conversation_id  bigint      NOT NULL REFERENCES conversations (id) ON DELETE CASCADE,
  user_id          bigint      NOT NULL REFERENCES users (id) ON DELETE CASCADE,
  acknowledged_at  timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (message_id, user_id)
);

-- One row per recipient of an urgent DM or group DM who has not acknowledged it yet. The scheduler
-- pushes again when `next_push_at` has passed and deletes the row after the last push.
CREATE TABLE urgent_reminders (
  message_id       bigint      NOT NULL,
  conversation_id  bigint      NOT NULL REFERENCES conversations (id) ON DELETE CASCADE,
  user_id          bigint      NOT NULL REFERENCES users (id) ON DELETE CASCADE,
  -- Pushes attempted so far, the first (immediate) one included.
  pushes_sent      integer     NOT NULL,
  next_push_at     timestamptz NOT NULL,
  PRIMARY KEY (message_id, user_id)
);
CREATE INDEX urgent_reminders_due_idx ON urgent_reminders (next_push_at);
