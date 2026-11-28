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
      adb exec-out run-as com.velmux.mobile.fixtures tar -C files -cf - evidence > "build/android-screenshots/api-$api/fixture-evidence.tar" || true
      if [ "$result" -eq 0 ]; then
        python3 scripts/ci/verify-mobile-outputs.py android "build/android-screenshots/api-$api/fixture-evidence.tar" "$api" || result=1
      fi
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
    bash scripts/ffmpeg/build-ios.sh
    bash scripts/ci/verify-native-ios.sh
    ;;
  *) echo 'Usage: scripts/test.sh host|android|ios-simulator' >&2; exit 2 ;;
esac
