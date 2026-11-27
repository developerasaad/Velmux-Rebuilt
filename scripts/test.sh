#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
case "${1:-}" in
  host) bash scripts/ci/verify-host.sh ;;
  android)
    bash scripts/ffmpeg/build-android.sh arm64-v8a x86_64
    bash scripts/ci/stage-android-runtime.sh
    ./gradlew -PbindingsTarget=android :android:connectedDebugAndroidTest
    ;;
  ios-simulator)
    bash scripts/ffmpeg/build-ios.sh
    ./gradlew -PbindingsTarget=ios :UI:linkDebugFrameworkIosSimulatorArm64
    mkdir -p build/ios/iphonesimulator
    rm -rf build/ios/iphonesimulator/VelmuxUI.framework
    cp -R UI/build/bin/iosSimulatorArm64/debugFramework/VelmuxUI.framework build/ios/iphonesimulator/
    device=$(python3 scripts/ci/ios-simulator.py)
    xcrun simctl boot "$device" || true
    xcrun simctl bootstatus "$device" -b
    xcodebuild -project platforms/ios/Velmux.xcodeproj -scheme Velmux -configuration Debug \
      -sdk iphonesimulator -destination "platform=iOS Simulator,id=$device" \
      -derivedDataPath build/xcode/tests -resultBundlePath build/ios-tests.xcresult \
      CODE_SIGNING_ALLOWED=NO test
    ;;
  *) echo 'Usage: scripts/test.sh host|android|ios-simulator' >&2; exit 2 ;;
esac
