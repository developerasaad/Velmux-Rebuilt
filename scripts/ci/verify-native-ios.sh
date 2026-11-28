#!/usr/bin/env bash
# Renew affected iOS regressions against the native SwiftUI/direct Swift host.
# This does not remove or substitute for the preserved prior-architecture gate.
set -euo pipefail
cd "$(dirname "$0")/../.."
root="$PWD"
[ "${CI:-}" = true ] && [ "$(uname -s)" = Darwin ] && [ "$(uname -m)" = arm64 ] || {
  echo 'Native iOS runtime verification requires a provisioned arm64 macOS CI runner.' >&2; exit 1;
}
bash scripts/check-build-environment.sh
evidence="$root/build/native-ios"
derived="$root/build/xcode/native/iphonesimulator/Debug"
result_bundle="$evidence/tests.xcresult"
mkdir -p "$evidence"
[ ! -e "$result_bundle" ] || { echo 'Preserve the existing result bundle and use a clean CI workspace for a new verification run.' >&2; exit 1; }
git rev-parse HEAD > "$evidence/source-commit.txt"
xcodebuild -version > "$evidence/xcode-toolchain.txt"
xcrun swiftc --version > "$evidence/swift-toolchain.txt"
if [ ! -f build/media-fixtures/fixtures.json ]; then bash scripts/ci/prepare-media-fixtures.sh; fi
# The workflow provisions/reuses the proven native dependencies. This builds the
# current Rust target once and reuses that exact output for XCTest compilation.
bash scripts/ci/apple-native-app.sh iphonesimulator Debug
core="$root/build/ios/iphonesimulator/Debug"
cp "$core/TARGET-IDENTITY" "$core/native-link-flags.json" "$core/native-app.xcconfig" "$evidence/"
device=$(python3 scripts/ci/ios-simulator.py)
printf '%s\n' "$device" > "$evidence/simulator-id.txt"
xcrun simctl list devices available --json > "$evidence/simulator-inventory.json"
xcrun simctl boot "$device" || true
xcrun simctl bootstatus "$device" -b
capture_result() {
  result=$?
  trap - EXIT
  if [ -f "$evidence/source-hashes.json" ]; then
    python3 scripts/ci/seed-ios-fixtures.py --verify "$evidence/source-hashes.json" || result=1
  fi
  if [ -d "$result_bundle" ]; then
    xcrun xcresulttool export attachments --path "$result_bundle" --output-path "$evidence/attachments" || true
  fi
  if [ "$result" -eq 0 ]; then
    if container=$(xcrun simctl get_app_container "$device" com.velmux.mobile data); then
      python3 scripts/ci/verify-mobile-outputs.py ios "$container" || result=1
    else
      result=1
    fi
  fi
  exit "$result"
}
trap capture_result EXIT
xcodebuild -project ios/Velmux.xcodeproj -scheme Velmux -configuration Debug \
  -sdk iphonesimulator -destination "platform=iOS Simulator,id=$device" \
  -derivedDataPath "$derived" -xcconfig "$core/native-app.xcconfig" \
  -parallel-testing-enabled NO CODE_SIGNING_ALLOWED=NO build-for-testing \
  2>&1 | tee "$evidence/build-for-testing.log"
products="$derived/Build/Products/Debug-iphonesimulator"
xcrun simctl install "$device" "$products/Velmux.app"
xcrun simctl install "$device" "$products/VelmuxMediaFixtures.app"
fixtures_container=$(xcrun simctl get_app_container "$device" com.velmux.mediafixtures data)
python3 scripts/ci/seed-ios-fixtures.py "$fixtures_container" build/media-fixtures "$evidence/source-hashes.json"
xcrun simctl launch "$device" com.velmux.mediafixtures
xcrun simctl terminate "$device" com.velmux.mediafixtures
xcodebuild -project ios/Velmux.xcodeproj -scheme Velmux -configuration Debug \
  -sdk iphonesimulator -destination "platform=iOS Simulator,id=$device" \
  -derivedDataPath "$derived" -xcconfig "$core/native-app.xcconfig" \
  -resultBundlePath "$result_bundle" -parallel-testing-enabled NO \
  CODE_SIGNING_ALLOWED=NO test-without-building 2>&1 | tee "$evidence/tests.log"
