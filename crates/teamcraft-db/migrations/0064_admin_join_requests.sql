-- Issue #776: a workspace admin asks the owners of a private channel to let them in
-- (admin.md §2.4, `admin_self_join = owner_approval`). The server only stores and relays the
-- request; an owner's client decides and adds the admin with its own MLS commit. One open request
-- per admin and channel: a decided or withdrawn request is deleted (the audit log keeps what
-- happened), an expired one by the account housekeeping.
CREATE TABLE admin_join_requests (
  conversation_id bigint      NOT NULL REFERENCES conversations (id) ON DELETE CASCADE,
  user_id         bigint      NOT NULL REFERENCES users (id) ON DELETE CASCADE,
  requested_at    timestamptz NOT NULL DEFAULT now(),
  expires_at      timestamptz NOT NULL,
  PRIMARY KEY (conversation_id, user_id),
  CHECK (expires_at > requested_at)
);
CREATE INDEX admin_join_requests_user_idx ON admin_join_requests (user_id);
CREATE INDEX admin_join_requests_expires_idx ON admin_join_requests (expires_at);
