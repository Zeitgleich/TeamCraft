-- Call history and missed calls (PLAN.md §6).

CREATE TABLE call_history (
  call_id         bigint PRIMARY KEY,
  conversation_id bigint NOT NULL REFERENCES conversations (id) ON DELETE CASCADE,
  started_by      bigint REFERENCES users (id) ON DELETE SET NULL,
  started_at      timestamptz NOT NULL DEFAULT now(),
  ended_at        timestamptz,
  -- The call rang (DMs and group DMs).
  was_ringing     boolean NOT NULL,
  -- Somebody other than the starter joined.
  was_answered    boolean NOT NULL DEFAULT false,
  -- The members were told about the missed call.
  missed_notified boolean NOT NULL DEFAULT false
);
CREATE INDEX call_history_conversation_idx ON call_history (conversation_id, call_id DESC);

CREATE TABLE call_history_participants (
  call_id bigint NOT NULL REFERENCES call_history (call_id) ON DELETE CASCADE,
  user_id bigint NOT NULL REFERENCES users (id) ON DELETE CASCADE,
  PRIMARY KEY (call_id, user_id)
);
CREATE INDEX call_history_participants_user_idx ON call_history_participants (user_id);
