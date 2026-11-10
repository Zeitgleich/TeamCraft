-- Health alerts and the notices about them survive a restart (#1646, docs/operations.md "Health and
-- alerts"). The server keeps both in memory as the source, because the database may be the check
-- that failed, and writes them here behind it. Both tables are bounded: alerts by age and count
-- (`history_days`, at most 500), notices by age (a day) and count (200).

-- One alert of one check: raised at a time, resolved at a later one or still open.
CREATE TABLE health_alerts (
  check_id    text        NOT NULL,
  raised_at   timestamptz NOT NULL,
  resolved_at timestamptz,
  PRIMARY KEY (check_id, raised_at)
);

CREATE INDEX health_alerts_raised_at ON health_alerts (raised_at DESC);

-- A raised or resolved alert on its way to one channel: an owner's or admin's address, or the
-- webhooks. A row goes once the delivery was made, so a restart sends only what was still waiting.
CREATE TABLE health_alert_notices (
  id              bigint      PRIMARY KEY,
  check_id        text        NOT NULL,
  kind            text        NOT NULL CHECK (kind IN ('raised', 'resolved')),
  changed_at      timestamptz NOT NULL,
  channel         text        NOT NULL CHECK (channel IN ('mail', 'webhook')),
  mail_address    text,
  queued_at       timestamptz NOT NULL,
  next_attempt_at timestamptz NOT NULL,
  failures        integer     NOT NULL CHECK (failures >= 0),
  CHECK ((channel = 'mail') = (mail_address IS NOT NULL))
);
