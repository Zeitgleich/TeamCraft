#!/usr/bin/env bash
# Debug files of release builds (docs/release.md, "Debug symbols"). The shipped binaries carry no
# debug info and no symbol table; their debug files are kept privately, by build ID, so crash reports
# and stack samples of a shipped build can be symbolized later. Never as a workflow artifact: this
# repository is public, and so are its artifacts.
#
#   scripts/debug-files.sh split BINARY OUT_DIR
#       ELF only: moves BINARY's debug info and symbol table to OUT_DIR/<name>.debug (compressed),
#       strips BINARY in place and links it to the debug file (.gnu_debuglink). Fails without a
#       GNU build ID: nothing could find the debug file again.
#   scripts/debug-files.sh store LABEL FILE...
#       Keeps debug files (ELF .debug or unstripped .so, .dSYM bundles, .pdb) wherever configured:
#         - the crash reports backend (GlitchTip/Sentry API) when SENTRY_URL, SENTRY_AUTH_TOKEN,
#           SENTRY_ORG and SENTRY_PROJECT are set: it symbolizes native crash reports by debug ID;
#         - a private S3 bucket when DEBUG_FILES_S3_ENDPOINT, DEBUG_FILES_S3_BUCKET,
#           DEBUG_FILES_S3_ACCESS_KEY_ID and DEBUG_FILES_S3_SECRET_ACCESS_KEY are set
#           (DEBUG_FILES_S3_REGION, default us-east-1): the place to download them from.
#       LABEL (e.g. desktop/1.0.32/x86_64-unknown-linux-gnu) names the index entry that lists the
#       build's files by version. A failed upload warns: the release goes on without it.
#   scripts/debug-files.sh fetch BUILD_ID [DIR]
#       Downloads an ELF debug file from the bucket to DIR/.build-id/<xx>/<rest>.debug (DIR defaults
#       to ~/.cache/teamcraft-debug), where `eu-stack --debuginfo-path=DIR`, gdb's
#       `debug-file-directory` and `eu-addr2line --debuginfo-path=DIR` find it by build ID.
#   scripts/debug-files.sh index LABEL
#       Prints a build's index (e.g. desktop/1.0.32/x86_64-unknown-linux-gnu).
#   scripts/debug-files.sh sentry-cli ARGS...
#       Runs the pinned sentry-cli (e.g. upload-proguard for the Android R8 mapping).
#
# S3 layout (the lookup layout each platform's tools already use):
#   buildid/<build-id>/debuginfo           ELF, as a debuginfod server serves it
#   macho/<UUID>/<name>.dSYM.zip           macOS dSYM bundle
#   pdb/<name>.pdb/<GUID><age>/<name>.pdb  Windows PDB, as a symbol server (symstore) serves it
#   index/<LABEL>.txt                      "<kind> <id> <key>" per file, plus the commit
set -euo pipefail

# Pinned sentry-cli: it reads the debug IDs and uploads (also the Android R8 mapping).
SENTRY_CLI_VERSION=3.8.2

fail() { echo "error: $*" >&2; [ -n "${GITHUB_ACTIONS:-}" ] && echo "::error::$*"; exit 1; }
warn() { echo "warning: $*" >&2; [ -n "${GITHUB_ACTIONS:-}" ] && echo "::warning::$*"; return 0; }

main() {
  case "${1:-}" in
    split) shift; [ "$#" -eq 2 ] || fail "usage: $0 split BINARY OUT_DIR"; split "$@" ;;
    store) shift; [ "$#" -ge 2 ] || fail "usage: $0 store LABEL FILE..."; store "$@" ;;
    fetch) shift; [ "$#" -ge 1 ] || fail "usage: $0 fetch BUILD_ID [DIR]"; fetch "$@" ;;
    sentry-cli) shift; install_sentry_cli; exec "$SENTRY_CLI" "$@" ;;
    index) shift; [ "$#" -eq 1 ] || fail "usage: $0 index LABEL"; require_bucket; s3_get "index/$1.txt" - ;;
    *) fail "usage: $0 split BINARY OUT_DIR | store LABEL FILE... | fetch BUILD_ID [DIR] | index LABEL" ;;
  esac
}

split() {
  local binary="$1" out_dir="$2" build_id debug_file
  build_id="$(elf_build_id "$binary")"
  [ -n "$build_id" ] || fail "$binary has no GNU build ID"
  mkdir -p "$out_dir"
  debug_file="$out_dir/$(basename "$binary").debug"
  objcopy --only-keep-debug --compress-debug-sections=zlib "$binary" "$debug_file"
  # Same result as Cargo's `strip = "symbols"`, plus the 4 KiB-aligned .gnu_debuglink section.
  objcopy --strip-all --add-gnu-debuglink="$debug_file" "$binary"
  echo "$binary: build ID $build_id, debug file $debug_file"
}

store() {
  local label="$1" file has_backend=false has_s3=false
  shift
  for file in "$@"; do [ -e "$file" ] || fail "no debug file at $file"; done
  if [ -n "${SENTRY_URL:-}" ] && [ -n "${SENTRY_AUTH_TOKEN:-}" ] \
    && [ -n "${SENTRY_ORG:-}" ] && [ -n "${SENTRY_PROJECT:-}" ]; then
    has_backend=true
  fi
  has_bucket && has_s3=true
  if [ "$has_backend" = false ] && [ "$has_s3" = false ]; then
    warn "no crash reports backend and no DEBUG_FILES_S3_* bucket: the debug files of $label are not kept"
    return 0
  fi
  install_sentry_cli
  if [ "$has_backend" = true ]; then
    "$SENTRY_CLI" debug-files upload "$@" \
      || warn "the debug files of $label did not reach the crash reports backend"
  fi
  if [ "$has_s3" = true ]; then
    store_in_bucket "$label" "$@" || warn "the debug files of $label did not reach the bucket"
  fi
}

fetch() {
  local build_id="$1" dir="${2:-$HOME/.cache/teamcraft-debug}" target
  [[ "$build_id" =~ ^[0-9a-f]{8,}$ ]] || fail "'$build_id' is not a build ID (lower-case hex)"
  require_bucket
  target="$dir/.build-id/${build_id:0:2}/${build_id:2}.debug"
  mkdir -p "$(dirname "$target")"
  s3_get "buildid/$build_id/debuginfo" "$target.part" \
    || { rm -f "$target.part"; fail "no debug file for build ID $build_id in the bucket"; }
  mv "$target.part" "$target"
  echo "$target"
}

require_bucket() {
  has_bucket || fail "set DEBUG_FILES_S3_ENDPOINT, _BUCKET, _ACCESS_KEY_ID and _SECRET_ACCESS_KEY (and _REGION)"
}

has_bucket() {
  [ -n "${DEBUG_FILES_S3_ENDPOINT:-}" ] && [ -n "${DEBUG_FILES_S3_BUCKET:-}" ] \
    && [ -n "${DEBUG_FILES_S3_ACCESS_KEY_ID:-}" ] && [ -n "${DEBUG_FILES_S3_SECRET_ACCESS_KEY:-}" ]
}

store_in_bucket() {
  local label="$1" file index line
  shift
  index="$(mktemp)"
  echo "commit ${GITHUB_SHA:-unknown}" > "$index"
  for file in "$@"; do
    line="$(store_file "$file")" || return 1
    [ -z "$line" ] || echo "$line" >> "$index"
  done
  s3_put "$index" "index/$label.txt" || return 1
  cat "$index"
  rm -f "$index"
}

# Uploads one debug file under its platform's key; prints its index line.
store_file() {
  local file="$1" check kind id name key zip
  check="$("$SENTRY_CLI" debug-files check --json "$file")" || return 1
  kind="$(json_field type <<<"$check")"
  # A stripped library (a prebuilt one next to ours) has nothing to symbolize with.
  if ! grep -Eq '"features": *"[^"]*(symtab|debug)' <<<"$check"; then
    echo "$file: no symbols or debug info, skipped" >&2
    return 0
  fi
  case "$kind" in
    elf)
      # sentry-cli counts .dynsym as a symbol table; a shipped (stripped) binary must never take
      # the place of its debug file.
      if ! readelf -S -W "$file" 2>/dev/null | grep -Eq ' \.(symtab|debug_info) '; then
        echo "$file: stripped, skipped" >&2
        return 0
      fi
      id="$(json_field code_id <<<"$check")"
      [ -n "$id" ] || { echo "$file: no build ID, skipped" >&2; return 0; }
      key="buildid/$id/debuginfo"
      s3_put "$file" "$key" || return 1
      ;;
    macho | dsym)
      # One architecture per build (no universal binaries), so one UUID.
      id="$(json_field debug_id <<<"$check" | tr -d - | tr '[:lower:]' '[:upper:]')"
      name="$(basename "$file")"
      key="macho/$id/$name.zip"
      zip="$(mktemp -d)/$name.zip"
      ditto -c -k --keepParent "$file" "$zip" || return 1
      s3_put "$zip" "$key" || return 1
      ;;
    pdb)
      id="$(symstore_signature "$(json_field debug_id <<<"$check")")"
      name="$(basename "$file")"
      key="pdb/$name/$id/$name"
      s3_put "$file" "$key" || return 1
      ;;
    *) warn "$file: unsupported debug file type '$kind'"; return 1 ;;
  esac
  echo "$kind $id $key"
}

# Sentry's PDB debug ID is "<GUID>-<age>" with the age in hex; a symbol server's is the GUID in
# upper-case hex without dashes followed by the age.
symstore_signature() {
  local debug_id="$1" age guid
  age="${debug_id##*-}"
  guid="${debug_id%-*}"
  echo "$(tr -d - <<<"$guid" | tr '[:lower:]' '[:upper:]')$(tr '[:lower:]' '[:upper:]' <<<"$age")"
}

# First value of a string field in sentry-cli's JSON (no jq on every runner).
json_field() {
  sed -n "s/.*\"$1\": *\"\\([^\"]*\\)\".*/\\1/p" | head -n 1
}

s3_put() {
  local file="$1" key="$2"
  curl --fail --silent --show-error --retry 5 --retry-all-errors \
    --aws-sigv4 "aws:amz:${DEBUG_FILES_S3_REGION:-us-east-1}:s3" \
    --user "$DEBUG_FILES_S3_ACCESS_KEY_ID:$DEBUG_FILES_S3_SECRET_ACCESS_KEY" \
    --upload-file "$file" "${DEBUG_FILES_S3_ENDPOINT%/}/$DEBUG_FILES_S3_BUCKET/$key"
}

s3_get() {
  local key="$1" output="$2"
  curl --fail --silent --show-error --retry 5 \
    --aws-sigv4 "aws:amz:${DEBUG_FILES_S3_REGION:-us-east-1}:s3" \
    --user "$DEBUG_FILES_S3_ACCESS_KEY_ID:$DEBUG_FILES_S3_SECRET_ACCESS_KEY" \
    --output "$output" "${DEBUG_FILES_S3_ENDPOINT%/}/$DEBUG_FILES_S3_BUCKET/$key"
}

elf_build_id() {
  readelf -n "$1" | sed -n 's/^ *Build ID: *\([0-9a-f]*\)$/\1/p' | head -n 1
}

# The release asset for this runner, checked against its pinned SHA-256.
install_sentry_cli() {
  local asset sha256 dir
  case "$(uname -s)-$(uname -m)" in
    Linux-x86_64) asset=sentry-cli-Linux-x86_64
      sha256=c958865f9d4000183509ea21b1f04925d5390de6d8d87386b9e69f2b305070c3 ;;
    Linux-aarch64) asset=sentry-cli-Linux-aarch64
      sha256=b6e49fca868a36d20da2ada1832a0ce2f9ac7012c128207518b09d6b6c991f4d ;;
    Darwin-*) asset=sentry-cli-Darwin-universal
      sha256=2d1b118fa46960ba5e8e0cca9a6e197366905776a0ddc5a89c7d05a3908b77db ;;
    MINGW*-x86_64 | MSYS*-x86_64) asset=sentry-cli-Windows-x86_64.exe
      sha256=50009f23c49d6e9ff7624204c7035855303b49c5e326708ee783c1fdf783ac83 ;;
    MINGW*-aarch64 | MINGW*-arm64 | MSYS*-aarch64 | MSYS*-arm64) asset=sentry-cli-Windows-aarch64.exe
      sha256=3c197c61120e2d53a6ecfd9e7812d123bfdc598df1d569700bf5c3b936dfe891 ;;
    *) fail "no pinned sentry-cli for $(uname -s)-$(uname -m)" ;;
  esac
  dir="${RUNNER_TEMP:-$(mktemp -d)}/sentry-cli-$SENTRY_CLI_VERSION"
  SENTRY_CLI="$dir/$asset"
  [ -x "$SENTRY_CLI" ] && return 0
  mkdir -p "$dir"
  curl --fail --silent --show-error --location --retry 5 --retry-all-errors --output "$SENTRY_CLI" \
    "https://github.com/getsentry/sentry-cli/releases/download/$SENTRY_CLI_VERSION/$asset"
  [ "$(sha256_of "$SENTRY_CLI")" = "$sha256" ] || fail "checksum mismatch: $asset"
  chmod +x "$SENTRY_CLI"
}

sha256_of() {
  if command -v sha256sum >/dev/null; then sha256sum "$1"; else shasum -a 256 "$1"; fi | cut -d ' ' -f 1
}

main "$@"
