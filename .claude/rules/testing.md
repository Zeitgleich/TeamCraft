# Testing

## Where tests live

- Unit tests in `#[cfg(test)] mod tests { ... }` at the bottom of each file.
- Cross-crate and protocol tests in the crate's `tests/suite/` directory: one test binary per crate
  (`tests/suite/main.rs` with a `mod` line per file), so adding a test file adds no link step. Shared
  helpers live in `tests/common/` and are included from `main.rs`. Tests that need another target
  (wasm) stay separate files in `tests/`.
- Tests that need Postgres, Keycloak or S3 use the dev stack (`docker compose -f docker-compose.dev.yml`)
  and are behind the `integration` feature, so `cargo test --workspace` works without it.

## Test module layout

Test cases first, helpers last.

1. Imports
2. Constants (fixture IDs, sample data)
3. Test functions, grouped into sub-modules by category when a category has 2+ tests
   (`mod key_rotation { ... }`); max two levels of nesting
4. Helper functions (builders, fixtures, assertions) at the bottom

Shared helpers go in the parent `mod tests` so sub-modules reach them via `use super::*`.

## What to test

- Every error type and its mapping (e.g. to HTTP status codes).
- Wire-format stability: serialization of enums and structs in `teamcraft-proto` against fixed JSON /
  bytes, including unknown-variant handling.
- Crypto: round-trips, rejection of tampered data, wrong keys, replay, old epochs, unsigned or
  wrongly signed membership changes. Use known-answer test vectors where a spec provides them.
- Parsers: empty, truncated, oversized and malformed input.
- Permission logic: every role × action combination that matters, especially guests.

## What not to test

- Trivial getters and struct construction.
- External services (SIP providers, voice agent providers, APNs, FCM) are never called from tests;
  use local fakes.

## Running

```sh
cargo test --workspace --all-features                 # unit tests
cargo nextest run --workspace --profile ci           # what CI runs (plus `cargo test --doc`)
cargo test --workspace --features integration         # with the dev stack running
```

Tests must be deterministic: no sleeping for timing, inject clocks and RNGs where behaviour depends
on them.
