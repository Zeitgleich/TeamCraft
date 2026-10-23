-- Whether the user lets their desktop and web sessions report attention to hold phone pushes back
-- (notifications.md, "Attention"). NULL: never chose (on).
ALTER TABLE user_notification_settings ADD COLUMN uses_desktop_attention boolean;
