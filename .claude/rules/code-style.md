# Code style

## Rust

- Latest stable Rust, edition 2024. No MSRV guarantees beyond `rust-version` in `Cargo.toml`.
- Two spaces for indentation (`rustfmt.toml`). Run `cargo fmt --all` before committing.
- No `unsafe` code. If a dependency needs it, that's fine; our crates never do.
- Never use `println!`, `eprintln!` or `dbg!` outside tests; use `tracing` (`info!`, `warn!`,
  `error!`, `debug!`, `trace!`). Use structured fields: `info!(conversation_id = %id, "joined")`.
- Optional fields and variables use the `maybe_` prefix: `maybe_thread_id`, `maybe_avatar`.

## Language

- Everything in the repository is plain English: identifiers, comments, log and error messages, config
  keys, protocol and tool names, docs, tests and fixtures.
- The only non-English files are i18n translations. User-facing text is never hard-coded; it uses
  i18n keys (Fluent, `.ftl` files per language, English as the source language).
- No real customer, project or company names or internal hostnames anywhere; use placeholders.

## Types

Make illegal states unrepresentable.

- **IDs are newtypes**, never bare `u64`/`String`: `UserId`, `DeviceId`, `ConversationId`,
  `MessageId`. Generate the boilerplate with the shared macro in `teamcraft-proto`, don't hand-write it.
- **Enums instead of strings and bools.** `ConversationKind::Channel`, not `kind: "channel"`;
  `HistoryPolicy::FromJoin`, not `share_history: false` when more variants are conceivable.
- **Wire enums are forward compatible:** enums sent between client and server have an
  `Unknown(String)` (or `#[serde(other)] Unknown`) catch-all so older clients don't break on new
  variants. Never change existing serialized values without a migration plan.
- No `Option<Option<T>>` – use an enum. No `HashMap<K, ()>` – use a `HashSet`.
- Secrets use dedicated types that zeroize on drop and don't implement `Debug`/`Display` with content
  (see [security.md](security.md)).
- Units in types or names: `Duration` over `u64` seconds; `max_size_bytes`, not `max_size`.

## Errors

- Libraries: one `thiserror` error enum per module or feature, named `{Feature}Error`
  (`KeyBagError`, `SipRegisterError`). Variants carry context, not just strings.
- Binaries: `anyhow::Result` in `main` and top-level wiring only; add `.context(...)` at boundaries.
- No `unwrap()` / `expect()` outside tests. Exception: a true invariant, with a comment explaining why
  it can't fail (`// Invariant: ...`).
- Don't swallow errors. If an error is deliberately ignored, say why in a comment.

## Structure and reuse

- One responsibility per crate. Shared logic belongs in a library crate (`teamcraft-proto`,
  `teamcraft-core`, …), never copied between binaries.
- Crypto only in `teamcraft-crypto`; SQL only in the DB crate; wire types only in `teamcraft-proto`.
- Crates that run in the browser (`proto`, `crypto`, `media`, `core`, `ui`) must build for
  `wasm32-unknown-unknown`; keep platform specifics behind traits or `cfg` in one place.
- Functions do one thing. If a function needs a comment to separate its phases, split it.
- Prefer composition and small traits over large generic frameworks. No speculative abstractions:
  add the trait when the second implementation arrives.
- Every new dependency needs a one-line justification in the PR. Prefer well-maintained,
  widely used crates; `cargo deny` must pass.

## File layout (top-to-bottom reading order)

Organize for reading like a newspaper: headline first, details later.

1. Imports
2. Constants
3. The primary public type the file is about
4. Supporting types (sub-structs, DTOs, helper enums) – below the primary type
5. API types in order: Request, Response, Error
6. `impl` blocks after their types

Inside an `impl`:

1. Constructors (`new`, `from_*`, builders)
2. Public methods
3. Private helpers, each *below* the method that calls it; meaty helpers above leaf helpers

## Naming

- Error enums: `{Feature}Error`
- Request/response: `{Action}Request`, `{Action}Response`
- HTTP handlers: `{verb}_{noun}_handler` (`create_channel_handler`)
- Builders: `{Thing}Builder` with `set_*` setters and `build()`
- Booleans read as questions: `is_guest`, `has_attachments`, `can_post`

## Imports

- Groups in this order, separated by a blank line: `std`, external crates, workspace crates, `crate::`.
- Prefer `use` over inline fully qualified paths. Inline qualification only for one-offs or names that
  collide with the prelude (`Result`, `Error`).
- No wildcard imports, except in test modules and preludes.
- Same type name from two crates: alias with a suffix naming the source,
  `use teamcraft_proto::Role as RoleProto;`

## Comments and docs

- Doc comments (`///`) on public items explain *what* and *why*, not *how*.
- Comments explain intent, invariants and non-obvious decisions. Don't narrate the code.
- Reference design docs where a rule comes from: `// e2ee.md §4.6: only signed changes are committed`.

## Frontend and mobile

- UI: Leptos (Rust), shared between desktop (Tauri) and web. No TypeScript application code.
- iOS: Swift + SwiftUI, formatted with `swift-format`. Android: Kotlin + Jetpack Compose, `ktlint`.
  Mobile shells stay thin: logic belongs in `teamcraft-core` behind UniFFI.

## Markdown

- English. Tables are **space-padded** so columns align when read raw:

  ```markdown
  | Crate             | Purpose                 |
  |-------------------|-------------------------|
  | teamcraft-proto   | Wire types              |
  | teamcraft-crypto  | All cryptography        |
  ```
