# AGENTS.md

Guidance for humans and coding agents working in the TeamCraft repository.

## Project overview

TeamCraft is a self-hosted business team chat (Slack / Mattermost / Teams style) with calls, meetings
and an AI phone hotline, written in Rust. Everything is end-to-end encrypted; the server runs on an
untrusted VPS.

Read before changing behaviour:

| Document                                       | Content                                                       |
|------------------------------------------------|---------------------------------------------------------------|
| [ARCHITECTURE.md](ARCHITECTURE.md)             | Components, threat model, data model, Keycloak, calls         |
| [PLAN.md](PLAN.md)                             | Feature catalog with priorities, roadmap                      |
| [docs/e2ee.md](docs/e2ee.md)                   | Encryption, devices, backup, push, search, compliance         |
| [docs/admin.md](docs/admin.md)                 | Roles, admin actions without content access                   |
| [docs/telephony.md](docs/telephony.md)         | SIP trunk, AI hotline, agents                                 |
| [docs/notifications.md](docs/notifications.md) | Threads, unread counts, notifications, urgent                 |
| [docs/bridge.md](docs/bridge.md)               | Integration bridge: webhooks and logs into encrypted channels |
| [docs/meetings.md](docs/meetings.md)           | Meetings, invitations, guests and lobby                       |
| [docs/push-gateway.md](docs/push-gateway.md)   | Push gateway for store apps (own repo): server side, contract |
| [docs/operations.md](docs/operations.md)       | Running an instance; deployment: Zeitgleich/teamcraft-deploy  |
| [docs/release.md](docs/release.md)             | Versions and the release procedure                            |
| [docs/updates.md](docs/updates.md)             | Desktop self-update, package repositories, update policy      |

## Repository layout

```
teamcraft/
├── crates/
│   ├── teamcraft-proto/     # Wire types: API, gateway, SFU signaling (wasm-compatible)
│   ├── teamcraft-crypto/    # All cryptography: MLS, keys, key bag, rendezvous (wasm-compatible)
│   ├── teamcraft-media/     # RTP, jitter buffer, codecs, DAVE, video effects shaders
│   ├── teamcraft-core/      # Client logic shared by desktop, web and mobile
│   ├── teamcraft-desktop-native/ # The only crate with `unsafe`: native OS calls of the desktop shell
│   ├── teamcraft-server/    # REST API + gateway (binary)
│   └── teamcraft-sfu/       # Media forwarding (binary)
├── docs/                    # Design documents
└── .claude/rules/           # Detailed rules (code style, testing, API, security)
```

Planned crates (ARCHITECTURE.md §8) are added when their milestone starts, not before.

## Rules (summary)

Details: [.claude/rules/](.claude/rules/). The short version:

- **Clean, typed, reusable.** Newtypes for IDs, enums instead of strings and bools, no
  `Option<Option<T>>`, no stringly-typed state. Shared logic goes into a library crate, not copied.
- **Errors:** `thiserror` enums in libraries, `anyhow` only at the top of binaries. No `unwrap()` /
  `expect()` outside tests (except for documented invariants with a reason).
- **No `unsafe`** (`unsafe_code = "forbid"` workspace-wide). One exception: `teamcraft-desktop-native`
  holds the few OS calls with no safe crate API, behind a safe API (code-style.md, "Native FFI").
- **Logging:** `tracing` macros only, never `println!` / `eprintln!` / `dbg!` outside tests.
  **Never log plaintext message content, keys, tokens or passwords.**
- **Crypto lives only in `teamcraft-crypto`.** No hand-rolled primitives; use audited crates.
- **Database access lives only in the DB crate** (from M1: `teamcraft-db`), using compile-time checked
  `sqlx::query!` / `query_as!`.
- **Formatting:** `cargo fmt` (two-space indent, see `rustfmt.toml`). Lints: workspace lints in
  `Cargo.toml`, CI runs clippy with `-D warnings`.
- **File layout:** top-to-bottom reading order – constants, primary type, supporting types, impls;
  constructors, public methods, then private helpers below their callers.
- **Configurable by default:** policies and limits are settings with a sensible default, not
  hard-coded behaviour (see PLAN.md).
- **Robust beyond the happy path:** every feature plan and PR review goes through
  [robustness.md](.claude/rules/robustness.md) – network loss and change, offline queues, app
  lifecycle, hostile networks, multi-device, server restarts, large data, clock skew. Known gaps are
  tracked in [docs/robustness-audit.md](docs/robustness-audit.md).
- **English only.** Code, identifiers, comments, logs, error messages, config keys, tool names, docs,
  commit messages and test data are plain English. The only exception are the i18n translation files
  (e.g. `i18n/de.ftl`); every user-facing string goes through i18n.
- **No real names.** No customer, project or company names, internal hostnames or internal API paths
  in code, docs, examples or tests – use neutral placeholders (`Project A`, `example.com`).
- **Vendor-neutral.** Integrations (SIP trunks, voice AI, push, S3, identity provider) go behind
  interfaces/config; a specific vendor is an implementation, never a hard dependency of the design.
- **Design:** all UI work uses the `apple-design` skill, and emoji are never icons (user content
  only) – see [design.md](.claude/rules/design.md).
- **Markdown tables are space-padded** so columns align in plain text.

## Before you commit

```sh
cargo fmt --all
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo deny check
```

## Git workflow

- Work on **feature branches** (`feat/…`, `fix/…`, `docs/…`), one feature or milestone step each.
- **Commit every meaningful step** on the branch – a working sub-step, not every line. Each commit
  builds and passes `cargo fmt --check` and `clippy`.
- Pull requests of people with write access get **auto-merge (squash)** on opening
  (`.github/workflows/auto-merge.yml`): they merge once `ci-gate` passes. Turn it off on a PR that must
  wait: `gh pr merge --disable-auto <number>`.
- `main` only receives **squash merges** of reviewed branches; the squash commit message summarizes
  the feature (subject ≤ 72 chars, body explains what and why).
- **Releases are nightly** (03:00 Berlin, `nightly-release.yml`, [docs/release.md](docs/release.md)):
  finished PRs go to `main` as soon as they are done; PR titles become the release highlights, so write them for readers, and label them (`feature`, `bug`, `security`,
  `breaking`) so the release notes group them. Don't bump the version or tag by hand for a patch.
- **Merging:** `main` requires one check, `ci-gate` (`ci.yml`). With the `ci` label it passes only
  when the whole CI passed; without it CI is skipped and it passes at once. So auto-merge
  (`gh pr merge --auto --squash`) waits for CI exactly when the pull request asked for it. Admins
  bypass the rule (batch merges); the nightly workflow marks its bump commit (docs/release.md).
- Commit messages: imperative subject line (≤ 72 chars), body explains *why*. Sign off with
  `git commit -s` (Developer Certificate of Origin, as in CalCraft).
