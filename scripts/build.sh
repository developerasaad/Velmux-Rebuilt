#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
case "${1:-}" in
  host)
    bash scripts/ffmpeg/fetch.sh host
    cargo build --locked --manifest-path core/Cargo.toml
    ;;
  android)
    bash scripts/ffmpeg/build-android.sh arm64-v8a x86_64
    ./gradlew -PbindingsTarget=android :android:assembleRelease
    python3 scripts/ci/verify-apk.py platforms/android/build/outputs/apk/release/android-release-unsigned.apk
    ;;
  ios-device|ios-simulator)
    bash scripts/ffmpeg/build-ios.sh
    if [ "$1" = ios-device ]; then
      sdk=iphoneos; target=IosArm64; destination='generic/platform=iOS'
    else
      sdk=iphonesimulator; target=IosSimulatorArm64; destination='generic/platform=iOS Simulator'
    fi
    ./gradlew -PbindingsTarget=ios ":UI:linkReleaseFramework$target"
    mkdir -p "build/ios/$sdk"
    rm -rf "build/ios/$sdk/VelmuxUI.framework"
    cp -R "UI/build/bin/$( [ "$sdk" = iphoneos ] && echo iosArm64 || echo iosSimulatorArm64 )/releaseFramework/VelmuxUI.framework" "build/ios/$sdk/"
    xcodebuild -project platforms/ios/Velmux.xcodeproj -scheme Velmux -configuration Release \
      -sdk "$sdk" -destination "$destination" -derivedDataPath "build/xcode/$sdk" \
      CODE_SIGNING_ALLOWED=NO build
    ;;
  *) echo 'Usage: scripts/build.sh host|android|ios-device|ios-simulator' >&2; exit 2 ;;
esac
