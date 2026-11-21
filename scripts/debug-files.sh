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
#   scripts/debug-files.sh fetch-dsym UUID [DIR]
#       Downloads an iOS/macOS dSYM bundle from the bucket and unpacks it to DIR/<UUID>/ (DIR
#       defaults to ~/.cache/teamcraft-debug/dsym), where scripts/symbolicate-ios.sh finds it.
#   scripts/debug-files.sh store-web-names LABEL MAP...
#       Keeps the web app's function names maps (teamcraft-ui-names-<build>.json.gz, written by
#       teamcraft-web-bundle `--names-out`, #2228) in the bucket only: the crash reports backend
#       doesn't symbolicate wasm. scripts/symbolicate-web.py reads them.
#   scripts/debug-files.sh fetch-web-names BUILD [DIR]
#       Downloads the names map of a web build (the 16 hex digits in its file names) to DIR
#       (default ~/.cache/teamcraft-debug/web-names), where scripts/symbolicate-web.py finds it.
#   scripts/debug-files.sh index LABEL
#       Prints a build's index (e.g. desktop/1.0.32/x86_64-unknown-linux-gnu).
#   scripts/debug-files.sh sentry-cli ARGS...
#       Runs the pinned sentry-cli (e.g. upload-proguard for the Android R8 mapping).
#
# S3 layout (the lookup layout each platform's tools already use):
#   buildid/<build-id>/debuginfo           ELF, as a debuginfod server serves it
#   macho/<UUID>/<name>.dSYM.zip           macOS dSYM bundle
#   pdb/<name>.pdb/<GUID><age>/<name>.pdb  Windows PDB, as a symbol server (symstore) serves it
#   web-names/<build>.json.gz              names map of the web app's wasm files, by build
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
    fetch-dsym) shift; [ "$#" -ge 1 ] || fail "usage: $0 fetch-dsym UUID [DIR]"; fetch_dsym "$@" ;;
    store-web-names) shift; [ "$#" -ge 2 ] || fail "usage: $0 store-web-names LABEL MAP..."; store_web_names "$@" ;;
    fetch-web-names) shift; [ "$#" -ge 1 ] || fail "usage: $0 fetch-web-names BUILD [DIR]"; fetch_web_names "$@" ;;
    sentry-cli) shift; install_sentry_cli || exit 1; exec "$SENTRY_CLI" "$@" ;;
    index) shift; [ "$#" -eq 1 ] || fail "usage: $0 index LABEL"; require_bucket; s3_get "index/$1.txt" - ;;
    *) fail "usage: $0 split BINARY OUT_DIR | store LABEL FILE... | fetch BUILD_ID [DIR] | fetch-dsym UUID [DIR] | store-web-names LABEL MAP... | fetch-web-names BUILD [DIR] | index LABEL" ;;
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
  if ! install_sentry_cli; then
    warn "no sentry-cli: the debug files of $label are not kept"
    return 0
  fi
  if [ "$has_backend" = true ]; then
    # --wait: the backend assembles the chunks after the upload; without waiting, a file it drops
    # there (as GlitchTip did with the large dSYMs of the iOS app) still reads as uploaded.
    if "$SENTRY_CLI" debug-files upload --wait "$@"; then
      verify_on_backend "$label" "$@"
    else
      warn "the debug files of $label did not reach the crash reports backend"
    fi
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

fetch_dsym() {
  local uuid dir="${2:-$HOME/.cache/teamcraft-debug/dsym}" key target
  uuid="$(tr -d - <<<"$1" | tr '[:lower:]' '[:upper:]')"
  [[ "$uuid" =~ ^[0-9A-F]{32}$ ]] || fail "'$1' is not a dSYM UUID"
  require_bucket
  # The index names the key (macho/<UUID>/<name>.dSYM.zip); the name isn't known here.
  key="$(s3_list "macho/$uuid/" | head -n 1)"
  [ -n "$key" ] || fail "no dSYM with UUID $uuid in the bucket"
  target="$dir/$uuid"
  mkdir -p "$target"
  s3_get "$key" "$target/dsym.zip.part" || fail "could not download $key"
  unzip -q -o "$target/dsym.zip.part" -d "$target"
  rm -f "$target/dsym.zip.part"
  echo "$target"
}

store_web_names() {
  local label="$1" file build index
  shift
  if ! has_bucket; then
    warn "no DEBUG_FILES_S3_* bucket: the web names maps of $label are not kept"
    return 0
  fi
  index="$(mktemp)"
  echo "commit ${GITHUB_SHA:-unknown}" > "$index"
  for file in "$@"; do
    build="$(basename "$file")"
    build="${build#teamcraft-ui-names-}"
    build="${build%.json.gz}"
    [[ "$build" =~ ^[0-9a-f]{16}$ ]] || fail "$file is not named teamcraft-ui-names-<build>.json.gz"
    if s3_put "$file" "web-names/$build.json.gz"; then
      echo "web-names $build web-names/$build.json.gz" >> "$index"
    else
      warn "the web names map $file did not reach the bucket"
    fi
  done
  s3_put "$index" "index/$label.txt" || warn "the index of $label did not reach the bucket"
  cat "$index"
  rm -f "$index"
}

fetch_web_names() {
  local build="$1" dir="${2:-$HOME/.cache/teamcraft-debug/web-names}" target
  [[ "$build" =~ ^[0-9a-f]{16}$ ]] || fail "'$build' is not a web build ID (16 lower-case hex digits)"
  require_bucket
  target="$dir/teamcraft-ui-names-$build.json.gz"
  mkdir -p "$dir"
  s3_get "web-names/$build.json.gz" "$target.part" \
    || { rm -f "$target.part"; fail "no names map of web build $build in the bucket"; }
  mv "$target.part" "$target"
  echo "$target"
}

# Checks that the backend kept each uploaded file: it lists them by debug ID, newest first.
verify_on_backend() {
  local label="$1" file id listing missing=() files=() arg
  shift
  listing="$(backend_debug_ids)" || { warn "could not list the debug files of the crash reports backend"; return 0; }
  # `check` reads single files: a dSYM bundle (a directory) is checked by the DWARF files inside it.
  for arg in "$@"; do
    if [ -d "$arg" ]; then
      # -H: Cargo's `target/<triple>/release/<name>.dSYM` is a link to the bundle in `deps/`.
      while IFS= read -r file; do files+=("$file"); done < <(find -H "$arg" -type f -path '*/Contents/Resources/DWARF/*')
    else
      files+=("$arg")
    fi
  done
  # `${a[@]+"${a[@]}"}`: macOS bash 3.2 treats an empty array as unbound under `set -u`.
  for file in ${files[@]+"${files[@]}"}; do
    # Only a check: a file sentry-cli can't read must not fail the release job.
    id="$("$SENTRY_CLI" debug-files check --json "$file" | json_field debug_id | tr '[:upper:]' '[:lower:]')" || continue
    [ -n "$id" ] || continue
    grep -qx -- "$id" <<<"$listing" || missing+=("$(basename "$file") $id")
  done
  if [ "${#missing[@]}" -gt 0 ]; then
    warn "the crash reports backend did not keep debug files of $label: ${missing[*]} (docs/crash-reports.md, \"Reading an iOS report\")"
  fi
}

# The debug IDs of the newest 100 debug files on the backend, lower case, one per line.
backend_debug_ids() {
  local url="${SENTRY_URL%/}/api/0/projects/$SENTRY_ORG/$SENTRY_PROJECT/files/dsyms/" cursor="" headers
  headers="$(mktemp)"
  for _ in 1 2; do
    # The token goes in a header read from stdin, never on the command line.
    printf 'header = "Authorization: Bearer %s"\n' "$SENTRY_AUTH_TOKEN" \
      | curl --fail --silent --show-error --retry 3 --config - --dump-header "$headers" \
        "$url${cursor:+?cursor=$cursor}" \
      | grep -o '"debugId": *"[^"]*"' | sed 's/.*"\([^"]*\)"$/\1/' | tr '[:upper:]' '[:lower:]' \
      || { rm -f "$headers"; return 1; }
    cursor="$(sed -n 's/.*rel="next"; results="true"; cursor="\([^"]*\)".*/\1/p' "$headers")"
    [ -n "$cursor" ] || break
  done
  rm -f "$headers"
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

# The keys under PREFIX (S3 ListObjectsV2), one per line.
s3_list() {
  local prefix="$1"
  curl --fail --silent --show-error --retry 5 \
    --aws-sigv4 "aws:amz:${DEBUG_FILES_S3_REGION:-us-east-1}:s3" \
    --user "$DEBUG_FILES_S3_ACCESS_KEY_ID:$DEBUG_FILES_S3_SECRET_ACCESS_KEY" \
    "${DEBUG_FILES_S3_ENDPOINT%/}/$DEBUG_FILES_S3_BUCKET?list-type=2&prefix=$prefix" \
    | grep -o '<Key>[^<]*</Key>' | sed 's/<Key>\(.*\)<\/Key>/\1/'
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
    *) echo "error: no pinned sentry-cli for $(uname -s)-$(uname -m)" >&2; return 1 ;;
  esac
  dir="${RUNNER_TEMP:-$(mktemp -d)}/sentry-cli-$SENTRY_CLI_VERSION"
  SENTRY_CLI="$dir/$asset"
  [ -x "$SENTRY_CLI" ] && return 0
  mkdir -p "$dir"
  curl --fail --silent --show-error --location --retry 5 --retry-all-errors --output "$SENTRY_CLI.part" \
    "https://github.com/getsentry/sentry-cli/releases/download/$SENTRY_CLI_VERSION/$asset" || return 1
  if [ "$(sha256_of "$SENTRY_CLI.part")" != "$sha256" ]; then
    echo "error: checksum mismatch: $asset" >&2
    rm -f "$SENTRY_CLI.part"
    return 1
  fi
  chmod +x "$SENTRY_CLI.part"
  mv "$SENTRY_CLI.part" "$SENTRY_CLI"
}

# From stdin: with a path, GNU sha256sum escapes backslashes (Windows paths) and prefixes the line.
sha256_of() {
  if command -v sha256sum >/dev/null; then sha256sum <"$1"; else shasum -a 256 <"$1"; fi | cut -d ' ' -f 1
}

main "$@"
