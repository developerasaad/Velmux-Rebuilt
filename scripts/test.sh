#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
bash scripts/check-build-environment.sh "${1:-}"
case "${1:-}" in
  host) bash scripts/ci/verify-host.sh ;;
  android)
    if [ ! -f build/media-fixtures/fixtures.json ]; then bash scripts/ci/prepare-media-fixtures.sh; fi
    mkdir -p build
    capture_android() {
      result=$?
      adb logcat -d -v threadtime > build/android-logcat.txt 2>&1 || true
      api=$(adb shell getprop ro.build.version.sdk | tr -d '\r')
      mkdir -p "build/android-screenshots/api-$api"
      adb pull /sdcard/Android/data/com.velmux.mobile/files/screenshots "build/android-screenshots/api-$api/" || true
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
    ./gradlew :android-fixtures:assembleDebug
    adb install -r -t tests/android-fixtures/build/outputs/apk/debug/android-fixtures-debug.apk
    bash scripts/ffmpeg/build-android.sh arm64-v8a x86_64
    ./gradlew -PbindingsTarget=android :android:connectedDebugAndroidTest
    ;;
  ios-simulator)
    capture_ios() {
      result=$?
      if [ -d build/ios-tests.xcresult ]; then
        xcrun xcresulttool export attachments --path build/ios-tests.xcresult --output-path build/ios-screenshots || true
      fi
      exit "$result"
    }
    trap capture_ios EXIT
    if [ ! -f build/media-fixtures/fixtures.json ]; then bash scripts/ci/prepare-media-fixtures.sh; fi
    bash scripts/ffmpeg/build-ios.sh
    bash scripts/ci/apple-framework.sh iphonesimulator Debug
    device=$(python3 scripts/ci/ios-simulator.py)
    xcrun simctl boot "$device" || true
    xcrun simctl bootstatus "$device" -b
    xcodebuild -project platforms/ios/Velmux.xcodeproj -scheme Velmux -configuration Debug \
      -sdk iphonesimulator -destination "platform=iOS Simulator,id=$device" \
      -derivedDataPath build/xcode/tests -parallel-testing-enabled NO \
      CODE_SIGNING_ALLOWED=NO build-for-testing
    app=build/xcode/tests/Build/Products/Debug-iphonesimulator/Velmux.app
    xcrun simctl install "$device" "$app"
    container=$(xcrun simctl get_app_container "$device" com.velmux.mobile data)
    mkdir -p "$container/Documents/UITestSources"
    cp build/media-fixtures/video.mp4 build/media-fixtures/stereo.wav "$container/Documents/UITestSources/"
    xcodebuild -project platforms/ios/Velmux.xcodeproj -scheme Velmux -configuration Debug \
      -sdk iphonesimulator -destination "platform=iOS Simulator,id=$device" \
      -derivedDataPath build/xcode/tests -resultBundlePath build/ios-tests.xcresult -parallel-testing-enabled NO \
      CODE_SIGNING_ALLOWED=NO test-without-building 2>&1 | tee build/ios-tests.log
    python3 - <<'CHECK_IOS_LOG'
from pathlib import Path
if 'Uncaught Kotlin exception:' in Path('build/ios-tests.log').read_text(errors='replace'):
    raise SystemExit('The iOS host logged an uncaught Kotlin exception during runtime verification')
CHECK_IOS_LOG
    ;;
  *) echo 'Usage: scripts/test.sh host|android|ios-simulator' >&2; exit 2 ;;
esac
