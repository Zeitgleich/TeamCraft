-- Call recordings that run, for the audit log (issue #2117, e2ee.md §8.3).
--
-- The SFU reports when a device's signed recording notice starts or stops a recording. The server
-- keeps the running ones here, so a recording keeps its start (and the audit log one entry for
-- it) when the SFU or the server restarts in the middle of it, and the stop knows how long it
-- ran. Metadata only: call, device, who and when; never anything of the recording.
CREATE TABLE open_call_recordings (
  call_id           bigint      NOT NULL,
  device_id         bigint      NOT NULL,
  user_id           bigint      NOT NULL,
  conversation_id   bigint      NOT NULL REFERENCES conversations (id) ON DELETE CASCADE,
  started_at        timestamptz NOT NULL DEFAULT now(),
  -- The most devices in the call at once while it ran, as far as the reports said.
  peak_participants integer     NOT NULL CHECK (peak_participants >= 0),
  PRIMARY KEY (call_id, device_id)
);
