-- Issue #1011: the user's own notification level for conversations they never configured (and
-- whose space has no level either). NULL: never chose, the instance default applies
-- (`[notifications] default_level`).
ALTER TABLE user_notification_settings
  ADD COLUMN notification_level text CHECK (notification_level IN ('all', 'mentions', 'none'));
