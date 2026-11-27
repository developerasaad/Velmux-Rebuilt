#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
bash scripts/check-build-environment.sh "${1:-}"
case "${1:-}" in
  host) bash scripts/ci/verify-host.sh ;;
  android)
    mkdir -p build
    capture_android() {
      result=$?
      adb logcat -d -v threadtime > build/android-logcat.txt 2>&1 || true
      if [ "$result" -ne 0 ]; then
        adb root || true
        adb wait-for-device || true
        adb pull /data/tombstones build/android-tombstones || true
        python3 scripts/ci/symbolize-android.py > build/android-symbols.txt 2>&1 || true
        cat build/android-symbols.txt
        tail -n 250 build/android-logcat.txt
      fi
      exit "$result"
    }
    trap capture_android EXIT
    adb logcat -c
    bash scripts/ffmpeg/build-android.sh arm64-v8a x86_64
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
