#!/usr/bin/env bash
# Build the pinned upstream generator as a private tool, then inspect the actual
# target archive. No host FFmpeg build or global cargo installation is needed.
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
archive=${1:?Apple static archive required}
destination=${2:?Generated output directory required}
case "$archive" in /*) ;; *) archive="$PWD/$archive" ;; esac
case "$destination" in /*) ;; *) destination="$PWD/$destination" ;; esac
[ -s "$archive" ] || { echo 'The target Rust archive is missing.' >&2; exit 1; }
read_pin() {
  python3 - "$root/scripts/interop/versions.json" "$1" <<'PY'
import json, sys
print(json.load(open(sys.argv[1]))['uniffi'][sys.argv[2]])
PY
}
version=$(read_pin version)
commit=$(read_pin commit)
digest=$(read_pin sha256)
url=$(read_pin url)
python3 - "$root/core/Cargo.toml" "$version" <<'PY'
import pathlib, re, sys
text = pathlib.Path(sys.argv[1]).read_text()
version = re.escape(sys.argv[2])
if not re.search(r'^uniffi\s*=\s*(?:"=' + version + r'"|\{[^\n]*version\s*=\s*"=' + version + r'")', text, re.M):
    raise SystemExit('The engine and source-pinned UniFFI generator versions differ')
PY
tools="$root/build/tools"
source="$tools/uniffi-$commit"
download="$tools/downloads/uniffi-$commit.tar.gz"
mkdir -p "$tools/downloads"
verify_download() {
  python3 - "$download" "$digest" <<'PY'
import hashlib, pathlib, sys
path = pathlib.Path(sys.argv[1])
raise SystemExit(0 if path.is_file() and hashlib.sha256(path.read_bytes()).hexdigest() == sys.argv[2] else 1)
PY
}
if ! verify_download; then
  temporary="$download.part"
  trap 'rm -f "$temporary"' EXIT
  curl --fail --location --silent --show-error --retry 3 --connect-timeout 20 --max-time 180 "$url" -o "$temporary"
  python3 - "$temporary" "$digest" <<'PY'
import hashlib, pathlib, sys
if hashlib.sha256(pathlib.Path(sys.argv[1]).read_bytes()).hexdigest() != sys.argv[2]:
    raise SystemExit('Pinned UniFFI source checksum mismatch')
PY
  mv "$temporary" "$download"
  trap - EXIT
fi
if [ ! -f "$source/SOURCE-SHA256" ] || [ "$(cat "$source/SOURCE-SHA256")" != "$digest" ]; then
  staging=$(mktemp -d "$tools/uniffi-source.XXXXXX")
  trap 'rm -rf "$staging"' EXIT
  tar -xzf "$download" --strip-components=1 -C "$staging"
  [ -s "$staging/Cargo.lock" ] || { echo 'Upstream generator lockfile is missing.' >&2; exit 1; }
  python3 - "$staging/uniffi/Cargo.toml" "$version" <<'PY'
import pathlib, re, sys
text = pathlib.Path(sys.argv[1]).read_text()
if not re.search(r'^version\s*=\s*"' + re.escape(sys.argv[2]) + r'"\s*$', text, re.M):
    raise SystemExit('Pinned upstream UniFFI package version does not match the engine')
PY
  printf '%s\n' "$digest" > "$staging/SOURCE-SHA256"
  rm -rf "$source"
  mv "$staging" "$source"
  trap - EXIT
fi
# This is the upstream CLI example, not another Velmux engine crate. Keep its
# unmodified workspace lockfile and cache its ordinary host build separately.
cargo build --locked --release \
  --manifest-path "$source/examples/app/uniffi-bindgen-cli/Cargo.toml" \
  --bin uniffi-bindgen --target-dir "$tools/uniffi-target"
mkdir -p "$destination"
(
  cd "$root/core"
  "$tools/uniffi-target/release/uniffi-bindgen" generate \
    --library "$archive" --crate velmux_core --language swift \
    --metadata-no-deps --no-format \
    --config "$root/scripts/interop/uniffi.toml" --out-dir "$destination"
)
for file in VelmuxCore.swift VelmuxCoreFFI.h VelmuxCoreFFI.modulemap; do
  [ -s "$destination/$file" ] || { echo "Missing generated Swift binding: $file" >&2; exit 1; }
done
printf '%s\n' "$digest" > "$destination/GENERATOR-SHA256"
