-- E-mail digest of missed mentions (PLAN.md §5): content-free, per user, batched.
CREATE TABLE user_email_digest (
  user_id              bigint      PRIMARY KEY REFERENCES users (id) ON DELETE CASCADE,
  is_enabled           boolean     NOT NULL DEFAULT false,
  -- NULL: the instance default interval.
  interval_minutes     integer     CHECK (interval_minutes > 0),
  -- Mentions up to this message ID were looked at (mailed, or read in time).
  watermark_message_id bigint      NOT NULL DEFAULT 0,
  -- When the scheduler looks at this user next.
  next_check_at        timestamptz NOT NULL DEFAULT now(),
  last_sent_at         timestamptz,
  updated_at           timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX user_email_digest_due_idx ON user_email_digest (next_check_at) WHERE is_enabled;
