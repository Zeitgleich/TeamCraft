# Security rules

The server is untrusted (ARCHITECTURE.md §2). Code must be written so that a compromised server, a
malicious member or hostile input can't break confidentiality.

## Secrets

- Keys, tokens, passwords and decrypted content use wrapper types that:
  - zeroize on drop (`zeroize` / `secrecy`),
  - don't implement `Debug`/`Display` with the content (print `[redacted]`),
  - aren't `Clone` unless needed.
- **Never log** plaintext messages, file names, keys, tokens, passwords, push payloads or full IDs
  of secrets. Log IDs of public objects (conversation, message) only.
- No secrets in URLs or query strings (except the deliberate `#fragment` of share links, which never
  reaches the server).

## Cryptography

- Only in `teamcraft-crypto` (and DAVE in `teamcraft-media` via `davey`).
- No hand-rolled primitives or protocols. Use `openmls`, `davey`, RustCrypto / `ring` / `aws-lc-rs`
  primitives. Constructions that combine primitives (key bag, rendezvous, index segments) follow the
  design docs and get a review before merging.
- Constant-time comparison for MACs and secrets (`subtle`).
- Every design change to keys or protocols updates `docs/e2ee.md` in the same PR.

## Trust decisions

- Clients never trust the server's word for: identity keys, group membership changes, roles, policy.
  These are verified against signatures (e2ee.md §3, admin.md §2.3).
- The server never trusts client input: validate sizes, counts, IDs and permissions on every request.
- Membership and permission checks live in one place per side; don't re-implement them in handlers.

## Input handling

- Parsers and protocol handlers (SIP, SDP, RTP, MLS messages, webhooks, iCalendar) get tests for
  malformed and hostile input, and a fuzz target when they parse network data.
- Limits everywhere: message size, attachment size, batch size, rate limits – all configurable.

## Dependencies

- `cargo deny check` must pass (licenses, advisories, sources).
- New crypto or network-facing dependencies need explicit justification in the PR.
