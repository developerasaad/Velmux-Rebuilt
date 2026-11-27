#!/usr/bin/env bash
# Resolve the FFmpeg artifacts for a target: reuse when the recorded build
# identity matches the current tree, build when missing or stale, and fail
# loudly when neither is possible. FFmpeg is MANDATORY for Velmux app
# builds — there is no "build without FFmpeg" path.
#
# Contract:
#   scripts/ffmpeg/fetch.sh host
#   scripts/ffmpeg/fetch.sh android <abi> [<abi>...]
#   scripts/ffmpeg/fetch.sh ios
#
# Environment: VELMUX_FFMPEG_JOBS (default: nproc), plus the builder's
# own environment variables (ANDROID_NDK_HOME for android targets).
#
# Outputs: nothing on stdout; the artifacts land in
#   scripts/ffmpeg-builder/out/host
#   scripts/ffmpeg-builder/out/android/<abi>
#   scripts/ffmpeg-builder/out/ios/{iphoneos,iphonesimulator}
set -euo pipefail

SCRIPTS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=lib/common.sh
. "$SCRIPTS_DIR/lib/common.sh"
# The builder library provides the identity helpers and the canonical
# DOWNLOADS/SRC/OUT locations (VELMUX_* overrides honored).
# shellcheck source=../../scripts/ffmpeg-builder/lib.sh
. "$VELMUX_ROOT/$VELMUX_FFMPEG_BUILDER_DIR/lib.sh"

usage() {
  cat <<'EOF'
Usage: scripts/ffmpeg/fetch.sh <host|android|ios> [abi...]

Resolves (reuses or builds) the pinned FFmpeg artifacts for a target.
Artifacts are reused only when their recorded build identity matches the
current tree; stale or missing artifacts are rebuilt.

  host            Host artifacts for development testing
  android <abis>  Per-ABI Android artifacts (arm64-v8a, armeabi-v7a, x86_64)
  ios             Device + simulator slices and the xcframework inputs
EOF
}

[ $# -ge 1 ] || { usage >&2; vdie "no target given"; }
TARGET="$1"; shift

export VELMUX_FFMPEG_JOBS="${VELMUX_FFMPEG_JOBS:-$(nproc 2>/dev/null || sysctl -n hw.ncpu 2>/dev/null || echo 2)}"
BUILDER="$VELMUX_ROOT/$VELMUX_FFMPEG_BUILDER_DIR"

# artifact_ok <dir> <expected-identity> — complete AND identity-matched.
# Reports the EXACT unmet condition (which file is missing, or the expected
# vs recorded identity) so a failed gate is diagnosable from the log alone
# instead of a bare "missing or stale".
artifact_ok() {
  local dir="$1" identity="$2"
  local lib
  for lib in avcodec avformat avutil avfilter swscale swresample x264 x265 \
             mp3lame opus ogg vorbis vpx dav1d SvtAv1Enc freetype png16 \
             harfbuzz fribidi ass zimg webp; do
    if [ ! -f "$dir/lib/lib$lib.a" ]; then
      vwarn "artifact check: missing $dir/lib/lib$lib.a"
      return 1
    fi
  done
  if [ ! -f "$dir/lib/pkgconfig/libavcodec.pc" ]; then
    vwarn "artifact check: missing $dir/lib/pkgconfig/libavcodec.pc"
    return 1
  fi
  if [ ! -f "$dir/share/velmux/ffmpeg-capabilities.json" ]; then
    vwarn "artifact check: missing capability manifest ($dir/share/velmux/ffmpeg-capabilities.json)"
    return 1
  fi
  if [ ! -f "$dir/BUILD-IDENTITY" ]; then
    vwarn "artifact check: missing BUILD-IDENTITY ($dir/BUILD-IDENTITY)"
    return 1
  fi
  if [ "$(identity_recorded "$dir")" != "$identity" ]; then
    vwarn "artifact check: identity mismatch for $dir"
    vwarn "  expected identity: $identity"
    vwarn "  recorded identity: $(identity_recorded "$dir")"
    # The recorded provenance (built-date, description with the build-time
    # knobs) narrows down WHEN and WITH WHAT KNOBS the artifact was made.
    while IFS= read -r line; do
      [ -n "$line" ] && vwarn "  recorded provenance: $line"
    done < <(sed -n '2,4p' "$dir/BUILD-IDENTITY" 2>/dev/null)
    return 1
  fi
  return 0
}

# Restored artifacts (CI artifact download, moved caches) may live at a
# different absolute path than where they were built; the .pc prefix lines
# must point at the real location before anything links against them.
refresh_prefixes() {
  local dir
  for dir in "$@"; do
    [ -d "$dir" ] && fixup_pkgconfig_prefix "$dir"
  done
}

case "$TARGET" in
  host)
    IDENTITY="$(compute_identity host)"
    HOST_OUT="$OUT_DIR/host"
    if artifact_ok "$HOST_OUT" "$IDENTITY"; then
      vlog "FFmpeg host artifacts up to date (identity verified)"
      refresh_prefixes "$HOST_OUT"
    else
      vlog "building FFmpeg host artifacts (missing or stale)"
      bash "$BUILDER/build-host.sh"
      artifact_ok "$HOST_OUT" "$IDENTITY" \
        || vdie "host FFmpeg build finished but the artifact is incomplete"
      vlog "FFmpeg host artifacts ready"
    fi
    ;;

  android)
    [ $# -ge 1 ] || vdie "fetch.sh android requires at least one ABI"
    # The NDK must be resolved BEFORE any identity computation: the recorded
    # identity includes the NDK revision, and the pre-check, the builder and
    # the final verification must all see the same one.
    resolve_android_ndk
    NEED_BUILD=()
    for abi in "$@"; do
      case " $VELMUX_ANDROID_ALL_ABIS " in
        *" $abi "*) ;;
        *) vdie "unknown Android ABI '$abi' (supported: $VELMUX_ANDROID_ALL_ABIS)" ;;
      esac
    done
    # Pre-check: report which ABIs are stale before invoking the builder
    # (which rebuilds exactly those).
    for abi in "$@"; do
      IDENTITY="$(compute_identity android "abi=$abi" "api=${VELMUX_ANDROID_API:-29}" "svt=$(case "$abi" in armeabi-v7a) echo 0 ;; *) echo 1 ;; esac)")"
      if artifact_ok "$OUT_DIR/android/$abi" "$IDENTITY"; then
        vlog "FFmpeg android/$abi up to date (identity verified)"
        refresh_prefixes "$OUT_DIR/android/$abi"
      else
        vlog "FFmpeg android/$abi missing or stale; will build"
        NEED_BUILD+=("$abi")
      fi
    done
    if [ ${#NEED_BUILD[@]} -gt 0 ]; then
      vlog "building FFmpeg android artifacts for: ${NEED_BUILD[*]}"
      bash "$BUILDER/build-android.sh" "${NEED_BUILD[@]}"
    fi
    # Final verification: every requested ABI must now be valid.
    for abi in "$@"; do
      IDENTITY="$(compute_identity android "abi=$abi" "api=${VELMUX_ANDROID_API:-29}" "svt=$(case "$abi" in armeabi-v7a) echo 0 ;; *) echo 1 ;; esac)")"
      artifact_ok "$OUT_DIR/android/$abi" "$IDENTITY" \
        || vdie "FFmpeg android/$abi artifact is missing or stale after the build"
      refresh_prefixes "$OUT_DIR/android/$abi"
    done
    vlog "FFmpeg android artifacts ready: $*"
    ;;

  ios)
    case "$(uname -s)" in
      Darwin) ;;
      *) vdie "the iOS FFmpeg artifacts can only be built on macOS (Xcode required)" ;;
    esac
    DEVICE_IDENTITY="$(compute_identity ios "slice=device-arm64" "deployment=$VELMUX_IOS_DEPLOYMENT_TARGET" "svt=1")"
    SIM_IDENTITY="$(compute_identity ios "slice=sim-arm64" "deployment=$VELMUX_IOS_DEPLOYMENT_TARGET" "svt=1")"
    if artifact_ok "$OUT_DIR/ios/iphoneos" "$DEVICE_IDENTITY" \
       && artifact_ok "$OUT_DIR/ios/iphonesimulator" "$SIM_IDENTITY"; then
      vlog "FFmpeg iOS artifacts up to date (identity verified)"
      refresh_prefixes "$OUT_DIR/ios/iphoneos" "$OUT_DIR/ios/iphonesimulator"
    else
      vlog "building FFmpeg iOS artifacts (missing or stale)"
      # The expected identities are passed to the builder: it records
      # exactly what this gate verifies (single source of truth) and
      # cross-asserts them against its own computation, failing fast with
      # a full input dump if the environment diverges between the two
      # computations (observed once on CI; see build_one_slice).
      VELMUX_IOS_EXPECTED_DEVICE_IDENTITY="$DEVICE_IDENTITY" \
      VELMUX_IOS_EXPECTED_SIM_ARM64_IDENTITY="$SIM_IDENTITY" \
      VELMUX_IOS_DEPLOYMENT_TARGET="$VELMUX_IOS_DEPLOYMENT_TARGET" \
        bash "$BUILDER/build-ios.sh"
      artifact_ok "$OUT_DIR/ios/iphoneos" "$DEVICE_IDENTITY" \
        || vdie "iOS device FFmpeg artifacts missing or stale after the build"
      artifact_ok "$OUT_DIR/ios/iphonesimulator" "$SIM_IDENTITY" \
        || vdie "iOS simulator FFmpeg artifacts missing or stale after the build"
      refresh_prefixes "$OUT_DIR/ios/iphoneos" "$OUT_DIR/ios/iphonesimulator"
      vlog "FFmpeg iOS artifacts ready"
    fi
    ;;

  *)
    usage >&2
    vdie "unknown target '$TARGET' (host, android, ios)"
    ;;
esac
