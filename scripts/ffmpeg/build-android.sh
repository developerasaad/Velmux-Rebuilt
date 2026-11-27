#!/usr/bin/env bash
# Build (or identity-verified reuse of) the Android FFmpeg artifacts.
#
# Contract: scripts/ffmpeg/build-android.sh [abi...]
#   Default ABI set: the release policy default (arm64-v8a). The emulator
#   test job requests x86_64 explicitly; pass several ABIs or use
#   ./scripts/build.sh --ffmpeg-android --all-abis for everything.
set -euo pipefail

SCRIPTS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=lib/common.sh
. "$SCRIPTS_DIR/lib/common.sh"

if [ $# -gt 0 ]; then
  ABIS="$*"
else
  ABIS="$VELMUX_ANDROID_DEFAULT_ABI"
fi
for abi in $ABIS; do
  case " $VELMUX_ANDROID_ALL_ABIS " in
    *" $abi "*) ;;
    *) vdie "unknown Android ABI '$abi' (supported: $VELMUX_ANDROID_ALL_ABIS)" ;;
  esac
done

bash "$SCRIPTS_DIR/ffmpeg/fetch.sh" android $ABIS
