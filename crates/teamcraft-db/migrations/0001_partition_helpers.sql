-- Helpers for tables partitioned by month on a Snowflake ID column.
--
-- Snowflake IDs are time-sortable (ARCHITECTURE.md §4.1), so partitioning by ID range is partitioning
-- by creation time while the primary key stays the plain ID.
-- Layout: 41 bits ms since the custom epoch | 10 bits worker | 12 bits sequence.

-- Smallest Snowflake ID created at or after `ts`. Epoch 2026-01-01T00:00:00Z = 1767225600000 ms.
CREATE FUNCTION tc_snowflake_floor(ts timestamptz) RETURNS bigint
  LANGUAGE sql IMMUTABLE STRICT PARALLEL SAFE
  RETURN (floor(extract(epoch FROM ts) * 1000)::bigint - 1767225600000) << 22;

-- Creates the monthly partitions of `parent` for `months` months starting with the month of `start_ts`.
-- Idempotent. Fails if the parent's default partition already holds rows in a requested range.
CREATE FUNCTION tc_ensure_month_partitions(parent text, start_ts timestamptz, months integer) RETURNS void
  LANGUAGE plpgsql AS $$
DECLARE
  first_month timestamptz := date_trunc('month', start_ts AT TIME ZONE 'UTC') AT TIME ZONE 'UTC';
  lower_bound timestamptz;
  upper_bound timestamptz;
BEGIN
  FOR i IN 0..months - 1 LOOP
    lower_bound := first_month + make_interval(months => i);
    upper_bound := first_month + make_interval(months => i + 1);
    EXECUTE format(
      'CREATE TABLE IF NOT EXISTS %I PARTITION OF %I FOR VALUES FROM (%s) TO (%s)',
      parent || '_' || to_char(lower_bound AT TIME ZONE 'UTC', 'YYYY_MM'),
      parent,
      tc_snowflake_floor(lower_bound),
      tc_snowflake_floor(upper_bound)
    );
  END LOOP;
END;
$$;
