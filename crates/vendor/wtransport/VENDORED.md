# Vendored: wtransport

| Item         | Value                                                                       |
|--------------|-----------------------------------------------------------------------------|
| Upstream     | https://github.com/BiagioFesta/wtransport (crate `wtransport` on crates.io) |
| Version      | 0.7.2, upstream commit `d022a6ad5f6260cb526e06d8ca42d09516ce6440`           |
| License      | MIT OR Apache-2.0 (`LICENSE-MIT`, `LICENSE-APACHE`, from that commit)       |
| Used by      | `teamcraft-media` (native call driver), `teamcraft-sfu`; `[patch.crates-io]` |
| Why vendored | Client `Endpoint::rebind` (issue #139); sending `CLOSE_WEBTRANSPORT_SESSION` (#459) |

## Why

A call client that switches networks (Wi-Fi to mobile, a VPN coming up) has to move its QUIC
connection to the new network (ARCHITECTURE.md §6.5). quinn can do that with
`Endpoint::rebind(socket)`: the connection continues on a fresh UDP socket, quinn switches to a new
connection ID and probes, and the SFU validates the new path. `wtransport` 0.7.2 keeps its
`quinn::Endpoint` private and offers no rebind for client endpoints (servers only have
`reload_config(.., rebind)`). Without it the old socket stays in use, and when that socket can't
reach the SFU from the new network (it was invalidated with its interface, or its address family is
gone), the call has to rejoin on a new connection with a DAVE re-key.

## Changes against upstream 0.7.2

The first commit adding this directory is the unmodified published crate (its normalized
`Cargo.toml`, plus the licence files from the upstream repository); `git log -p --
crates/vendor/wtransport` shows every change since.

1. `src/endpoint.rs`: `Endpoint<endpoint_side::Client>::rebind(std::net::UdpSocket)`, forwarding to
   `quinn::Endpoint::rebind`.
2. Sending the session close (#459; 0.7.2 can only receive it): `Connection::close_session(code,
   reason)` asks the driver's worker to write a `CLOSE_WEBTRANSPORT_SESSION` capsule (type 0x2843:
   32-bit code, reason up to 1024 bytes) in a DATA frame on the CONNECT stream and finish it, and
   returns once the peer has acknowledged it. `src/driver/mod.rs` (request channel to the worker),
   `src/driver/streams/connect.rs` (`send_close`, encoding, tests), `src/connection.rs`. Also
   `Connection::closed()`: when the QUIC connection ended locally because the peer's capsule
   arrived, it reports that capsule's code and reason as `ConnectionError::ApplicationClosed`
   instead of `LocallyClosed`, so a native client still learns the SFU's going-away code.
3. `Cargo.toml`: an empty `[workspace]` table so the copy builds and tests on its own (our workspace
   excludes `crates/vendor`).
4. Added `VENDORED.md`, `UPSTREAM_PR.md` with `upstream.patch` (change 1, relative to the upstream
   repository), `.gitignore` (standalone `Cargo.lock` and `target/`).

No other source changes. The capsule encoding is covered by unit tests in `connect.rs`
(run `cargo test --lib` inside this directory) and end to end by `teamcraft-sfu`'s
`tests/suite/session_close.rs`.

## Updating or dropping the vendor copy

- **Upstream releases a client rebind** (e.g. after the PR in `UPSTREAM_PR.md`): delete
  `crates/vendor/wtransport` and the `[patch.crates-io] wtransport` entry in the root `Cargo.toml`,
  bump the `wtransport` requirement in `teamcraft-media` and `teamcraft-sfu` to that release, adapt
  `call_native.rs` if the method is named differently, then `cargo update -p wtransport` and run the
  workspace checks.
- **Upstream releases a newer version without it:** re-vendor it in two commits (unmodified copy,
  then the changes above) so the diff stays reviewable.
- Keep the version in `Cargo.toml` equal to upstream's, otherwise `[patch.crates-io]` no longer
  matches the crates' requirement.
