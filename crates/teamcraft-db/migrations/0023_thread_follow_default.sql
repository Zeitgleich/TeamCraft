-- Auto-follow of mentioned threads becomes "follow the workspace default" until a user chooses
-- (signed policy `threads.auto_follows_mentioned_threads`, admin.md §4.1). Everybody who still has
-- the former default (true) goes back to following it; an explicit "off" is kept.
ALTER TABLE users ALTER COLUMN auto_follow_mentioned_threads DROP NOT NULL;
ALTER TABLE users ALTER COLUMN auto_follow_mentioned_threads DROP DEFAULT;
UPDATE users SET auto_follow_mentioned_threads = NULL WHERE auto_follow_mentioned_threads;
