-- Issue #776, review of #797: a decided request stays until it would have expired. The approving
-- owner's retry is recognised (and nobody else's), an admin can't ask, withdraw and ask again to
-- notify the owners over and over, and the approval is one transaction with the membership.
ALTER TABLE admin_join_requests
  ADD COLUMN state text NOT NULL DEFAULT 'open'
    CHECK (state IN ('open', 'approved', 'declined', 'withdrawn')),
  ADD COLUMN decided_by bigint REFERENCES users (id) ON DELETE SET NULL,
  ADD COLUMN decided_at timestamptz;
