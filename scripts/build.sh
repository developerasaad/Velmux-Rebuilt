#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
bash scripts/check-build-environment.sh "${1:-}"
case "${1:-}" in
  host)
    bash scripts/ffmpeg/fetch.sh host
    cargo build --locked --manifest-path core/Cargo.toml
    ;;
  android)
    . scripts/lib/common.sh
    resolve_android_ndk
    bash scripts/ffmpeg/build-android.sh arm64-v8a x86_64
    ./gradlew -PbindingsTarget=android :android:assembleRelease
    python3 scripts/ci/verify-apk.py android/build/outputs/apk/release/android-release-unsigned.apk
    ;;
  ios-device|ios-simulator)
    bash scripts/ffmpeg/build-ios.sh
    if [ "$1" = ios-device ]; then
      sdk=iphoneos
    else
      sdk=iphonesimulator
    fi
    bash scripts/ci/apple-native-app.sh "$sdk" Release
    ;;
  *) echo 'Usage: scripts/build.sh host|android|ios-device|ios-simulator' >&2; exit 2 ;;
esac
