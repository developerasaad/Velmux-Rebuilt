#!/usr/bin/env bash
# Print the FFmpeg build identity for a target — the same hash fetch.sh
# verifies before reusing artifacts. CI uses it as the exact cache key, so
# "cache reuse" and "artifact identity" can never disagree (spec §13).
#
# Contract:
#   scripts/ffmpeg/identity.sh host
#   scripts/ffmpeg/identity.sh android <abi>
#   scripts/ffmpeg/identity.sh ios <device|simulator>
set -euo pipefail

SCRIPTS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=lib/common.sh
. "$SCRIPTS_DIR/lib/common.sh"
# The identity helpers live in the builder library.
# shellcheck source=../../scripts/ffmpeg-builder/lib.sh
. "$VELMUX_ROOT/$VELMUX_FFMPEG_BUILDER_DIR/lib.sh"

case "${1:-}" in
  host)
    compute_identity host
    ;;
  android)
    [ $# -ge 2 ] || vdie "identity.sh android requires an ABI"
    case "$2" in
      arm64-v8a|armeabi-v7a|x86_64) ;;
      *) vdie "unknown Android ABI '$2'" ;;
    esac
    # Match the knobs fetch.sh feeds the identity for Android targets.
    compute_identity android "abi=$2" "api=${VELMUX_ANDROID_API:-29}" \
      "svt=$(case "$2" in armeabi-v7a) echo 0 ;; *) echo 1 ;; esac)"
    ;;
  ios)
    [ $# -ge 2 ] || vdie "identity.sh ios requires a slice (device|simulator)"
    case "$2" in
      device) compute_identity ios "slice=device-arm64" "deployment=$VELMUX_IOS_DEPLOYMENT_TARGET" "svt=1" ;;
      simulator) compute_identity ios "slice=sim-arm64" "deployment=$VELMUX_IOS_DEPLOYMENT_TARGET" "svt=1" ;;
      *) vdie "identity.sh ios slice must be device or simulator" ;;
    esac
    ;;
  *)
    vdie "usage: identity.sh <host|android <abi>|ios <device|simulator>>"
    ;;
esac
