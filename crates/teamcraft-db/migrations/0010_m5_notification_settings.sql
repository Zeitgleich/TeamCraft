-- Milestone 5: notification preferences (PLAN.md §5).

ALTER TABLE memberships
  -- Mutes the conversation until this time (NULL: not muted by time).
  ADD COLUMN muted_until timestamptz;

CREATE TABLE user_notification_settings (
  user_id             bigint PRIMARY KEY REFERENCES users (id) ON DELETE CASCADE,
  -- 'away_only': no push while a device of the user is active; 'always': push regardless.
  push_mode           text    NOT NULL DEFAULT 'away_only' CHECK (push_mode IN ('away_only', 'always')),
  -- NULL: the instance default applies.
  send_read_receipts  boolean,
  -- JSON of the typed DND schedule; validated by the server, opaque to the database.
  dnd_schedule        text,
  updated_at          timestamptz NOT NULL DEFAULT now()
);
