# Vendored: openmls_memory_storage

| Item         | Value                                                                      |
|--------------|----------------------------------------------------------------------------|
| Upstream     | https://github.com/openmls/openmls, `memory_storage` (crate on crates.io)  |
| Version      | 0.6.0, upstream commit `3a3e35de3feeca8f6605143c464d5452ae584d43`          |
| License      | MIT (`LICENSE`, copied from the upstream repository)                       |
| Used by      | `openmls_rust_crypto` for every provider, via `[patch.crates-io]`          |
| Why vendored | A save must know what changed without comparing the whole state (#840);    |
|              | a start must not read every group's state first (#1758)                    |

## Why

`teamcraft-core` writes the openmls provider storage to the local store, one record per entry
(docs/e2ee.md §9.5). The storage is a map whose keys only this crate can build: a wrapper around the
`StorageProvider` trait (72 methods) would have to repeat the key of each one, and a key wrong by a
byte would silently leave an entry unsaved, which can lose a group. Finding the changes without a
wrapper means keeping a second copy of the state and comparing all of it on every save (#840): memory
the iOS notification extension doesn't have, old secrets kept longer, and work that grows with the
number of groups on a hot path.

So the crate itself notes the keys it changes, and the bytes it stores stay as they were.

## Changes against upstream 0.6.0

The first commit adding this directory is the unmodified crate (its normalized `Cargo.toml`, without
`Cargo.toml.orig` and cargo's bookkeeping files); `git log -p -- crates/vendor/openmls_memory_storage`
shows every change since.

1. `Cargo.toml`: an empty `[workspace]` table, so the copy builds and tests on its own (our workspace
   excludes `crates/vendor`).
2. `src/lib.rs`: every write takes `TrackedValues` instead of the map's write lock. It derefs to the
   map for reading and has only `insert`, `remove` and `list_bytes` to change it, so a write that
   doesn't note its key doesn't compile. New public methods: `take_changed_keys`, `has_changed_keys`,
   `set_value`, `remove_value`. Keys and values are stored exactly as before.

   Noting starts with the first `take_changed_keys`: a provider nobody saves (DAVE's, for the length
   of a call) keeps no set of keys. `teamcraft-core` calls it when it loads a state; a save from a new
   `SavedMlsStorage` writes everything anyway, and starts it there.

   The public `values` field stays, so code outside this crate can still change the map past the
   guard. Two places do: `teamcraft-core` fills it when it loads a state (nothing has changed against
   the store then), and `Session::reset` in the vendored `davey` calls `values.write().clear()`, which
   is harmless: nobody saves a DAVE provider, and it notes nothing. Writes that must be saved use
   `set_value` and `remove_value`.

   `insert` and `remove` wipe the value they replace (new dependency `zeroize`, already in our tree):
   it costs a write over the few kilobytes of each changed entry, and old epoch secrets otherwise stay
   in freed memory. It is not a guarantee: the lists changed in place through `list_bytes`, dropping
   the whole map, `clear()`, and the copies openmls makes to serialize are not wiped.
3. Added `VENDORED.md`, `LICENSE`, and the `changed_keys` tests at the end of `src/lib.rs`.
4. `src/lib.rs`, loading on demand (issue #1758): `load_on_demand` gives the storage a
   `ValueSource` (`teamcraft-core` reads the local store), set once before use. A read through the
   storage trait, and `append` and `remove_item` before they change a list, first put a value only
   the source has in memory (`fault_in`), asking the source once per key. A key looked up there
   (found or not), written, deleted or filled in is "settled": never read from the source again, so
   a deleted value can't come back. On such a storage a delete is a change even when memory didn't
   hold the key (the source may), and writes keep noting their keys as before. `fill` lets the owner
   put values it read from the source itself (a listing) without overriding a settled key, and is
   no change; `is_on_demand` says which kind a storage is. New error `SourceUnreadable`. Reading the
   public `values` map directly sees only what is in memory; the iterations of the
   `virtual-clients-draft` feature (not enabled here) do too. A storage without a source behaves as
   before. Tests: `on_demand` at the end of `src/lib.rs` (`cargo test --features test-utils` here).

## Updating

When `openmls_rust_crypto` moves to a newer `openmls_memory_storage`, the patch stops applying and
`teamcraft-core` stops compiling (no `take_changed_keys`). Take the new upstream crate and repeat
changes 2 and 4. `what_was_saved_is_what_the_provider_holds_after_real_operations` and
`a_client_opened_on_demand_saves_what_real_operations_change` (`teamcraft-core`,
`tests/suite/mls_storage.rs`) check on real MLS operations that what was saved is what the provider
holds; a new read path of the storage trait that doesn't fault in fails the second.
