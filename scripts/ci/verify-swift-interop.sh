#!/usr/bin/env bash
# Temporary direct-binding acceptance scaffold. Keep the old production iOS
# bridge until native presentation and its complete affected regressions pass.
set -euo pipefail
cd "$(dirname "$0")/../.."
root="$PWD"
[ "${CI:-}" = true ] || { echo 'Run substantial direct Swift verification on a provisioned CI worker.' >&2; exit 1; }
[ "$(uname -s)" = Darwin ] && [ "$(uname -m)" = arm64 ] || {
  echo 'Direct Swift verification requires an actual arm64 macOS runner.' >&2; exit 1;
}
bash scripts/check-build-environment.sh
python3 scripts/interop/test-build-apple.py
evidence="$root/build/swift-interop"
mkdir -p "$evidence"
rustc -vV > "$evidence/rust-toolchain.txt"
xcodebuild -version > "$evidence/xcode-toolchain.txt"
xcrun swiftc --version > "$evidence/swift-toolchain.txt"
# The wrapper reuses only complete identity-matching dependencies. It does not
# construct a host FFmpeg prefix or invoke Kotlin/Gradle.
bash scripts/ffmpeg/build-ios.sh
for pair in 'iphoneos Release' 'iphonesimulator Debug'; do
  read -r sdk configuration <<< "$pair"
  bash scripts/interop/build-apple.sh "$sdk" "$configuration"
  directory="$root/build/ios/$sdk/$configuration"
  mkdir -p "$evidence/$sdk"
  cp -R "$directory/generated" "$evidence/$sdk/"
  cp "$directory/native-link-flags.json" "$directory/archive-architecture.txt" \
    "$directory/archive-members.txt" "$directory/artifact-sha256.txt" "$directory/rust-build.log" \
    "$directory/TARGET-IDENTITY" "$evidence/$sdk/"
  native_flags=()
  while IFS= read -r argument; do
    native_flags+=(-Xlinker "$argument")
  done < <(python3 - "$directory/native-link-flags.json" <<'PY'
import json, sys
for flag in json.load(open(sys.argv[1])):
    print(flag)
PY
  )
  case "$sdk" in
    iphoneos) swift_target=arm64-apple-ios15.0 ;;
    iphonesimulator) swift_target=arm64-apple-ios15.0-simulator ;;
  esac
  # Link the exact target staticlib only once. Its bundled FFmpeg closure must
  # not also be linked as separate archives or through the old VelmuxUI binary.
  xcrun --sdk "$sdk" swiftc -swift-version 5 -target "$swift_target" \
    -sdk "$(xcrun --sdk "$sdk" --show-sdk-path)" -parse-as-library \
    -module-name VelmuxDirectInteropSmoke \
    -Xcc "-fmodule-map-file=$directory/generated/VelmuxCoreFFI.modulemap" \
    "$directory/generated/VelmuxCore.swift" tests/interop/swift/Smoke.swift \
    ios/src/integration/NativeProviders.swift ios/src/media/IosMediaServices.swift \
    "$directory/libvelmux_core.a" "${native_flags[@]}" \
    -o "$evidence/$sdk/swiftInteropSmoke" 2>&1 | tee "$evidence/$sdk/swift-link.log"
  xcrun vtool -show-build "$evidence/$sdk/swiftInteropSmoke" > "$evidence/$sdk/macho-build-version.txt"
  xcrun otool -L "$evidence/$sdk/swiftInteropSmoke" > "$evidence/$sdk/dynamic-dependencies.txt"
  python3 - "$evidence/$sdk/macho-build-version.txt" "$evidence/$sdk/dynamic-dependencies.txt" "$sdk" <<'PY'
from pathlib import Path
import re, sys
build = Path(sys.argv[1]).read_text()
minimum = re.findall(r'\bminos\s+([0-9.]+)', build)
if len(minimum) != 1 or [int(part) for part in minimum[0].split('.')][0] != 15 or any(int(part) for part in minimum[0].split('.')[1:]):
    raise SystemExit('The actual Apple smoke binary does not retain iOS15.0: ' + repr(minimum))
expected = 'IOS' if sys.argv[3] == 'iphoneos' else 'IOSSIMULATOR'
if not re.search(r'\bPLATFORM\s+' + expected + r'\b', build.upper()):
    raise SystemExit('The Apple executable was linked for the wrong platform')
dependencies = Path(sys.argv[2]).read_text()
if re.search(r'VelmuxUI|Kotlin|libav(codec|format|filter|util)|libx26[45]', dependencies, re.I):
    raise SystemExit('The direct smoke unexpectedly depends on another engine/Kotlin/FFmpeg dylib')
PY
done
# Core exports are currently target-independent; prevent independently stale
# device/simulator generated APIs before any old bridge is removed.
for file in VelmuxCore.swift VelmuxCoreFFI.h VelmuxCoreFFI.modulemap; do
  cmp "$evidence/iphoneos/generated/$file" "$evidence/iphonesimulator/generated/$file"
done
device=$(python3 scripts/ci/ios-simulator.py)
printf '%s\n' "$device" > "$evidence/simulator-id.txt"
xcrun simctl boot "$device" || true
xcrun simctl bootstatus "$device" -b
xcrun simctl list devices available --json > "$evidence/simulator-inventory.json"
binary="$evidence/iphonesimulator/swiftInteropSmoke"
codesign --force --sign - "$binary"
runtime="$evidence/runtime-$(uuidgen | tr '[:upper:]' '[:lower:]')"
xcrun simctl spawn --arch=arm64 "$device" "$binary" "$runtime" 2>&1 | tee "$evidence/smoke.log"
python3 - "$runtime/proof.json" "$evidence/smoke.log" <<'PY'
import json, pathlib, sys
proof = json.loads(pathlib.Path(sys.argv[1]).read_text())
for field in ('single_engine', 'callbacks_released_after_engine', 'pre_call_cancellation',
              'mid_callback_cancellation', 'typed_core_and_callback_errors'):
    if proof.get(field) is not True:
        raise SystemExit('Missing executed direct Swift evidence: ' + field)
if proof['media']['codec'] != 'aac' or proof['media']['decoded_frames'] <= 0:
    raise SystemExit('The direct provider path did not produce independently decoded AAC')
if 'DIRECT_SWIFT_INTEROP_PASSED' not in pathlib.Path(sys.argv[2]).read_text():
    raise SystemExit('The direct Swift smoke process did not finish successfully')
print('Verified actual direct Swift/iOS simulator binding execution and real provider conversion.')
print('Production SwiftUI migration, physical devices and hardware acceleration remain unverified.')
PY
