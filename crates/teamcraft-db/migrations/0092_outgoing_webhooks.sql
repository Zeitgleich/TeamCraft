-- Outgoing webhooks (docs/webhooks.md): admins point the server at external URLs that are told
-- about channel events the server sees anyway. Metadata only; the payloads hold IDs and times.

-- One configured target. The signing secret is sealed with the settings key (as the mail and
-- calendar passwords are); the health columns back the console's status and the backoff.
CREATE TABLE webhooks (
  id                   bigint      PRIMARY KEY,
  url                  text        NOT NULL,
  description          text        NOT NULL DEFAULT '',
  -- Wire names of the subscribed events (`member.joined`, ...).
  events               text[]      NOT NULL,
  is_enabled           boolean     NOT NULL,
  sealed_secret        bytea       NOT NULL,
  created_at           timestamptz NOT NULL DEFAULT now(),
  updated_at           timestamptz NOT NULL DEFAULT now(),
  -- Failures in a row. While above zero, deliveries wait until `paused_until`: the backoff is per
  -- target, so a target that is down gets one attempt per backoff step, not one per queued event,
  -- and a recovered one is not flooded.
  consecutive_failures integer     NOT NULL DEFAULT 0,
  paused_until         timestamptz,
  last_success_at      timestamptz,
  last_failure_at      timestamptz,
  last_error           text,
  -- Events dropped: queue full, too old, out of attempts, or refused for good.
  dropped_count        bigint      NOT NULL DEFAULT 0
);

-- Events waiting for delivery, one row per webhook and event. A worker claims the oldest due row
-- together with its webhook (SELECT ... FOR UPDATE SKIP LOCKED) and keeps both locks while it
-- sends: one delivery per webhook is in flight at a time, and a crashed server releases the claim
-- with its connection, so a delivery is neither lost nor sent by two workers.
CREATE TABLE webhook_deliveries (
  webhook_id bigint      NOT NULL REFERENCES webhooks (id) ON DELETE CASCADE,
  -- The event's ID (a Snowflake, so in event order), sent as `webhook-id`: the same for all
  -- attempts and all webhooks.
  event_id   bigint      NOT NULL,
  event_kind text        NOT NULL,
  -- The JSON body, fixed at the time of the event so every attempt sends the same bytes.
  payload    text        NOT NULL,
  -- Attempts made so far.
  attempts   integer     NOT NULL DEFAULT 0,
  created_at timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (webhook_id, event_id)
);
CREATE INDEX webhook_deliveries_event_idx ON webhook_deliveries (event_id);
CREATE INDEX webhook_deliveries_created_idx ON webhook_deliveries (created_at);

-- The latest attempts per webhook for the console; trimmed to a fixed number per webhook.
CREATE TABLE webhook_delivery_log (
  id          bigint      PRIMARY KEY,
  webhook_id  bigint      NOT NULL REFERENCES webhooks (id) ON DELETE CASCADE,
  event_id    bigint      NOT NULL,
  event_kind  text        NOT NULL,
  attempt     integer     NOT NULL,
  outcome     text        NOT NULL,
  status_code integer,
  -- A status line or connection error; never a response body.
  error       text,
  duration_ms integer     NOT NULL,
  at          timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX webhook_delivery_log_webhook_idx ON webhook_delivery_log (webhook_id, id DESC);
