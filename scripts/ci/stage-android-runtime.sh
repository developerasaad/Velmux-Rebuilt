#!/usr/bin/env bash
# Package the C++ runtime required by the static FFmpeg dependency closure.
set -euo pipefail
cd "$(dirname "$0")/../.."
. scripts/lib/common.sh
resolve_android_ndk
for abi in arm64-v8a x86_64; do
  case "$abi" in
    arm64-v8a) triple=aarch64-linux-android ;;
    x86_64) triple=x86_64-linux-android ;;
  esac
  for toolchain in "$ANDROID_NDK_HOME"/toolchains/llvm/prebuilt/*; do
    runtime="$toolchain/sysroot/usr/lib/$triple/libc++_shared.so"
    if [ -f "$runtime" ]; then
      destination="platforms/android/build/generated/jniLibs/$abi"
      mkdir -p "$destination"
      cp "$runtime" "$destination/"
      break
    fi
  done
  [ -f "platforms/android/build/generated/jniLibs/$abi/libc++_shared.so" ] || vdie "C++ runtime missing for $abi"
done
