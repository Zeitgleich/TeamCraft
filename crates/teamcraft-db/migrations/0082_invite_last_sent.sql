-- When an invitation mail last went out, so resending can be limited (issue #298).
ALTER TABLE invites ADD COLUMN last_sent_at timestamptz NOT NULL DEFAULT now();
