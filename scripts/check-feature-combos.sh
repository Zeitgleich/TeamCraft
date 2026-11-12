#!/usr/bin/env bash
# Per-crate clippy on the feature sets the workspace really builds each crate with (issue #1774).
#
# `cargo clippy -p <crate>` resolves features for that crate alone, without the unification a
# `--workspace` run gets from the other crates. A backend-specific item that is only live with
# another feature then shows up as dead code. CI's workspace run (`--all-features`) can't see this,
# so this script runs the targeted checks: use it as the quick check for a change in one crate.
#
#   scripts/check-feature-combos.sh                    # every combination
#   scripts/check-feature-combos.sh teamcraft-core     # the combinations of one crate (or several)
#
# Needs libclang for the tests of teamcraft-core (sqlite-plugin): on macOS without Xcode, set
# LIBCLANG_PATH and DYLD_FALLBACK_LIBRARY_PATH to the Command Line Tools' usr/lib.
# Keep the list in step with the `teamcraft-core = { ..., features = [...] }` lines of the crates'
# Cargo.toml files: a new feature set of a dependency belongs here.
set -euo pipefail
cd "$(dirname "$0")/.."

# "<crate>|<targets>|<extra cargo args>": one clippy run each. `all` = --all-targets. The core's
# integration tests need the `ui` feature (`tests/common`), so a feature set without it is checked
# on the library only (likewise the server's tests and `integration`).
combos=(
  # Storage backends of the core, alone and together: the two stores share the store's SQL modules.
  "teamcraft-core|lib|--no-default-features"
  "teamcraft-core|lib|--no-default-features --features sqlcipher"
  "teamcraft-core|lib|--no-default-features --features sealed-sqlite"
  "teamcraft-core|lib|--no-default-features --features sqlcipher,sealed-sqlite"
  "teamcraft-core|lib|--no-default-features --features service-worker"
  # With the UI adapter, tests included: the default features (SQLCipher), and what the mobile
  # bindings (teamcraft-ffi) and the UI crate build the core with.
  "teamcraft-core|all|--no-default-features --features ui,sqlcipher"
  "teamcraft-core|all|--no-default-features --features ui,sealed-sqlite"
  "teamcraft-core|all|--no-default-features --features ui,sqlcipher,sealed-sqlite"
  "teamcraft-core|all|--features ui"
  # The other crates, each with the feature sets its own Cargo.toml picks for its dependencies.
  "teamcraft-ffi|all|"
  "teamcraft-ui|all|"
  # The server's integration tests need the `integration` feature (and skip without a database).
  "teamcraft-server|lib|"
  "teamcraft-server|all|--features integration"
  "teamcraft-media|all|"
  "teamcraft-sfu|all|"
)

wanted=("$@")
is_wanted() {
  [ ${#wanted[@]} -eq 0 ] && return 0
  local crate
  for crate in "${wanted[@]}"; do
    [ "$crate" = "$1" ] && return 0
  done
  return 1
}

failed=()
for combo in "${combos[@]}"; do
  crate="${combo%%|*}"
  rest="${combo#*|}"
  kind="${rest%%|*}"
  args="${rest#*|}"
  if [ "$kind" = all ]; then targets="--all-targets"; else targets="--lib"; fi
  is_wanted "$crate" || continue
  echo "==> cargo clippy -p $crate $targets $args"
  # shellcheck disable=SC2086
  if ! cargo clippy -p "$crate" $targets $args -- -D warnings; then
    failed+=("$crate $args")
  fi
done

if [ ${#failed[@]} -gt 0 ]; then
  echo
  echo "Failed combinations:"
  printf '  %s\n' "${failed[@]}"
  exit 1
fi
echo "All feature combinations pass."
