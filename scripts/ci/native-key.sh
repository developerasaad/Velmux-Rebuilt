#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/../.."
. scripts/lib/common.sh
case "$1" in
  host) key=$(bash scripts/ffmpeg/identity.sh host) ;;
  android)
    # Hosted images export their default NDK; select our pin explicitly.
    unset ANDROID_NDK_HOME ANDROID_NDK_ROOT
    resolve_android_ndk
    printf 'ANDROID_NDK_HOME=%s\n' "$ANDROID_NDK_HOME" >> "$GITHUB_ENV"
    key=$(bash scripts/ffmpeg/identity.sh android "$2")
    ;;
  ios)
    device=$(bash scripts/ffmpeg/identity.sh ios device)
    simulator=$(bash scripts/ffmpeg/identity.sh ios simulator)
    key="$device-$simulator"
    ;;
  *) exit 2 ;;
esac
printf 'key=%s\n' "$key" >> "$GITHUB_OUTPUT"
