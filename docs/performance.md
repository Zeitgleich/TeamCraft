# Performance and scale

What was measured, how to repeat it, and what the numbers mean for the targets in
ARCHITECTURE.md §1.1 ("huge channels stay cheap"). Both tests are `#[ignore]`d integration tests of
`teamcraft-server`; they use the real router, a real Postgres and real WebSocket connections, so
everything includes authentication, JSON and the database round trip.

```sh
export DATABASE_URL=postgres://teamcraft:teamcraft-dev-only@127.0.0.1:5432/teamcraft   # dev stack
cargo test -p teamcraft-server --features integration --test perf_history -- --ignored --nocapture
cargo test -p teamcraft-server --features integration --test perf_gateway -- --ignored --nocapture
```

Environment of the numbers below: one machine (24 cores, 62 GB RAM, NVMe) running the test (client
and server in one process) and PostgreSQL 18 in a container on the same host, so there is no network
latency in them. Treat them as the cost of the software, not as a promise for your hardware. The
tests assert generous bounds, so a regression fails them.

## A channel with 10 million messages

`perf_history`: 10,000,000 message rows (200 bytes of ciphertext each), spread over nine months
of monthly partitions, in one channel (`TEAMCRAFT_PERF_ROWS` changes the size). The table with its
indexes takes 3.6 GB; seeding takes about nine minutes with plain SQL.

| Operation (through the API)                                    | p50     | p95     | p99     |
|----------------------------------------------------------------|---------|---------|---------|
| Newest page of 50 messages                                     | 6.8 ms  | 7.1 ms  | 7.7 ms  |
| Page 100 pages (5,000 messages) back                           | 7.0 ms  | 7.4 ms  | 8.8 ms  |
| Forward page after a deep cursor                               | 6.2 ms  | 6.6 ms  | 6.9 ms  |
| Newest page for a member who just joined (`from_join` history) | 4.8 ms  | 5.7 ms  | 11.9 ms |
| Unread counts, user never read the channel (10M unread)        | 10 ms   | -       | -       |
| Unread counts, read position at the newest message             | 4.5 ms  | 7.5 ms  | 8.9 ms  |
| Send one message                                               | 10.7 ms | 16.4 ms | 18.7 ms |

All of these are dominated by the request overhead (about 4 ms for authentication and the user
lookup); the queries themselves take 0.3-0.5 ms (`EXPLAIN ANALYZE` is printed by the test). Paging
cost does not grow with the age of the cursor or the size of the table, because every query is an
index range scan on `(conversation_id, id)` that Postgres merges across the monthly partitions.

**Retention:** keeping the newer half of the data (`max_age_days` on the channel) deletes 5.0 million
rows and drops the five emptied partitions in 13.7 s; history paging is as fast afterwards. The job
deletes by ID range per conversation, so it is bounded by the number of rows to delete, and runs in
the background (`[retention]`).

### What the test found and fixed

- **JIT compilation.** The first runs of the unread-count query on a prepared connection took
  0.5 s at 10 million rows (and 50 ms at 3 million). The planner's cost estimate for a query over a
  table with a dozen partitions exceeds Postgres' JIT threshold, so the query was compiled instead
  of executed. The server now connects with `jit = off` (right for an OLTP workload); the same query
  takes 4 ms. If you connect through a pooler that ignores startup options, set `jit = off` on the
  database or the role.
- **Unbounded unread counts.** Counting unread messages for a user who never read a huge channel
  scanned all of it (7.4 s for 10 million). Counts now look at the newest `limits.max_unread_counted`
  (1000) unread messages per conversation; a count equal to the limit means "that many or more".
  Mentions are counted among those messages.

## Gateway fan-out

`perf_gateway`: 500 users with a device and a gateway connection each, all members of one channel;
a bot posts to it at 300 messages per minute for 30 seconds while a second, small channel
(two members) carries a message every half second. Settings: `TEAMCRAFT_PERF_CONNECTIONS`,
`TEAMCRAFT_PERF_BOT_PER_MINUTE`, `TEAMCRAFT_PERF_SECONDS`.

| What                                                                  | p50   | p95   | p99    |
|-----------------------------------------------------------------------|-------|-------|--------|
| Bot message to every one of the 500 connections (75,000 deliveries)   | 52 ms | 61 ms | 126 ms |
| `POST /messages` round trip of the bot (includes publishing to 500)   | 58 ms | 64 ms | 134 ms |
| Message in the small channel to its other member, without bot traffic | 17 ms | 21 ms | -      |
| The same while the bot posts to the 500-member channel                | 12 ms | 62 ms | 64 ms  |

All 75,000 deliveries arrived, none were missing or duplicated. The small channel did not slow down
under the bot's traffic (its p50 stayed where it was; the p95 rise to 62 ms is the same order as one
bot message's fan-out and comes from sharing the process with 500 sockets). Publishing is done on the
request path and costs about 0.1 ms per connection, so a 500-member channel adds about 50 ms to a
send. Delivery happens through in-memory queues per session; a slow client cannot hold up the others
(its buffer is bounded and it resumes or resyncs when it reconnects).

For larger channels the cost grows linearly: 5,000 connected members mean about half a second for
a send on the request path. If that becomes the limit, move publishing off the request path (the
event is already stored before it is published) before adding instances.

## Limits to keep in mind

- All of this is for a single server instance and one database; rate limits and the call roster are
  per instance (`docs/operations.md`).
- Index segments, attachments and search are client-side and not covered here.
- The load tests do not use TLS, a reverse proxy or real clients; WebSocket frames are small.
