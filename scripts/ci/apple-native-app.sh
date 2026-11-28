#!/usr/bin/env bash
# Build the native SwiftUI app from the same generated target-core artifacts as
# direct Swift verification. The established bridge remains a separate gate.
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
cd "$root"
verify_only=false
if [ "${1:-}" = --verify-inputs ]; then
  verify_only=true
  shift
fi
sdk=${1:?Usage: apple-native-app.sh [--verify-inputs] iphoneos\|iphonesimulator Debug\|Release}
configuration=${2:?Debug or Release required}
shift 2
case "$sdk" in
  iphoneos) destination='generic/platform=iOS' ;;
  iphonesimulator) destination='generic/platform=iOS Simulator' ;;
  *) echo 'Expected iphoneos or iphonesimulator.' >&2; exit 2 ;;
esac
case "$configuration" in Debug|Release) ;; *) echo 'Expected Debug or Release.' >&2; exit 2 ;; esac
core_directory="$root/build/ios/$sdk/$configuration"
if [ "$verify_only" = false ]; then
  [ "${CI:-}" = true ] || { echo 'Run substantial native Apple builds on a provisioned CI worker.' >&2; exit 1; }
  [ "$(uname -s)" = Darwin ] && [ "$(uname -m)" = arm64 ] || {
    echo 'The native Apple app requires an actual arm64 macOS runner.' >&2; exit 1;
  }
  bash scripts/check-build-environment.sh
  # Dependency restoration/construction is the caller's existing setup step.
  # Cargo reuses its ordinary target cache; no Kotlin framework is constructed.
  bash scripts/interop/build-apple.sh "$sdk" "$configuration"
fi
python3 - "$root" "$core_directory" "$sdk" "$configuration" "$verify_only" <<'PY'
import hashlib
import json
from pathlib import Path
import sys

root, directory = (Path(item).resolve() for item in sys.argv[1:3])
sdk, configuration = sys.argv[3:5]
verify_only = sys.argv[5] == 'true'

def digest(path):
    result = hashlib.sha256()
    with path.open('rb') as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b''):
            result.update(block)
    return result.hexdigest()

try:
    record = json.loads((directory / 'TARGET-IDENTITY').read_text())
    if (record['sdk'], record['configuration'], record['ios_minimum']) != (sdk, configuration, '15.0'):
        raise ValueError('Target identity does not match the requested app build')
    inputs = [root / relative for relative in ('core/Cargo.toml', 'core/Cargo.lock',
              'core/build.rs', 'rust-toolchain.toml')]
    for relative in ('core/src', 'scripts/interop'):
        inputs.extend(path for path in (root / relative).rglob('*') if path.is_file())
    if {str(path.relative_to(root)) for path in inputs} != set(record['source_sha256']):
        raise ValueError('Core or interoperability source files were added or removed after generation')
    for relative, expected in record['source_sha256'].items():
        if digest(root / relative) != expected:
            raise ValueError('Core or interoperability inputs changed after generation: ' + relative)
    for relative, expected in record['output_sha256'].items():
        if digest(directory / relative) != expected:
            raise ValueError('Generated target artifact changed: ' + relative)
    prefix = root / 'scripts/ffmpeg-builder/out/ios' / sdk
    for relative, field in [('BUILD-IDENTITY', 'native_build_record_sha256'),
                            ('share/velmux/ffmpeg-capabilities.json', 'native_capabilities_sha256')]:
        if digest(prefix / relative) != record[field]:
            raise ValueError('Native dependencies differ from the linked target artifacts')
    flags = json.loads((directory / 'native-link-flags.json').read_text())
    if not flags or not all(isinstance(flag, str) and flag and not any(character in flag for character in '\n\r\x00') for flag in flags):
        raise ValueError('The target reported invalid native linker flags')
    # Rust supplies only its non-bundled system closure. Never allow a second
    # engine/FFmpeg archive to enter through the generated system flags.
    if any(flag.endswith(('.a', '.framework')) or flag.startswith(('-L', '-lvelmux')) for flag in flags):
        raise ValueError('The native system closure unexpectedly includes an archive or search path')
except (OSError, ValueError, KeyError, TypeError) as error:
    raise SystemExit('Native app inputs are missing or stale. Run scripts/ci/apple-native-app.sh first. ' + str(error))

def quote(value):
    return '"' + value.replace('\\', '\\\\').replace('"', '\\"').replace('$', '\\$') + '"'

# Compile generated Swift directly in Velmux; only the C FFI is a Clang module.
# The archive appears exactly once in Xcode's target settings. Preserve each
# actual system argument separately rather than guessing a framework list.
config = ('// Generated from the verified Rust target identity; do not commit.\n'
          'VELMUX_NATIVE_INPUTS_READY = YES\n'
          'VELMUX_NATIVE_LINK_FLAGS = ' + ' '.join(quote(flag) for flag in flags) + '\n')
configuration_file = directory / 'native-app.xcconfig'
if verify_only and (not configuration_file.is_file() or configuration_file.read_text() != config):
    raise SystemExit('The native app configuration is missing or stale. Run scripts/ci/apple-native-app.sh first.')
if not verify_only and (not configuration_file.is_file() or configuration_file.read_text() != config):
    configuration_file.write_text(config)
PY
if [ "$verify_only" = true ]; then
  [ "${VELMUX_NATIVE_INPUTS_READY:-}" = YES ] || {
    echo 'Invoke the native app through scripts/ci/apple-native-app.sh so the reported system link closure is included.' >&2
    exit 1
  }
  exit 0
fi
build_directory="$root/build/xcode/native/$sdk/$configuration"
mkdir -p "$build_directory"
xcodebuild -project ios/Velmux.xcodeproj -scheme Velmux -configuration "$configuration" \
  -sdk "$sdk" -destination "$destination" \
  -derivedDataPath "build/xcode/native/$sdk/$configuration" \
  -xcconfig "$core_directory/native-app.xcconfig" \
  CODE_SIGNING_ALLOWED=NO "$@" build 2>&1 | tee "$build_directory/build.log"
