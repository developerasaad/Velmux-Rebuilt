#!/usr/bin/env bash
# Package the iOS FFmpeg build (out/ios/{iphoneos,iphonesimulator}) into the
# form the CocoaPods engine build consumes (Milestone 3 wiring):
#
#   <dest>/VelmuxFFmpeg.xcframework   static-library xcframework
#   <dest>/include                    FFmpeg + dependency public headers
#
# Each slice's static libraries are merged with `libtool -static` into ONE
# archive per slice, so the final application link has no cross-archive
# symbol-ordering hazards (a single archive is resolved through its symbol
# index). Slices may legitimately contain different library sets — the
# simulator slice omits SVT-AV1 — and each slice packages exactly what it
# built; fat inputs keep their per-architecture members.
#
# Usage:
#   package-ios-xcframework.sh <out-ios-root> <dest-root>
#
# <out-ios-root> is the directory produced by build-ios.sh (contains
# iphoneos/ and iphonesimulator/). Must run on macOS (Xcode toolchain).
set -euo pipefail

if [ "$#" -ne 2 ]; then
  echo "usage: $0 <out-ios-root> <dest-root>" >&2
  exit 2
fi

IOS_ROOT="$(cd "$1" && pwd)"
mkdir -p "$2"
DEST_ROOT="$(cd "$2" && pwd)"

. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"

case "$(uname -s)" in
  Darwin) ;;
  *) die "package-ios-xcframework.sh must run on macOS (Xcode toolchain required)" ;;
esac

require_tool libtool
require_tool xcodebuild

for slice in iphoneos iphonesimulator; do
  [ -d "$IOS_ROOT/$slice/lib" ] || die "missing slice directory: $IOS_ROOT/$slice/lib"
done

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

# --- Merge each slice into a single static archive -------------------------------------------
# CocoaPods requires every slice of a vendored static-library xcframework
# to expose the SAME binary name, so each slice's merged archive is
# libvelmux_ffmpeg.a inside its own slice directory.
declare -a slice_archives=()
for slice in iphoneos iphonesimulator; do
  libs=()
  while IFS= read -r lib; do
    libs+=("$lib")
  done < <(ls "$IOS_ROOT/$slice/lib/"*.a | sort)
  if [ "${#libs[@]}" -eq 0 ]; then
    die "[$slice] no static libraries found in $IOS_ROOT/$slice/lib"
  fi
  out="$work/$slice/libvelmux_ffmpeg.a"
  mkdir -p "$work/$slice"
  log "[$slice] merging ${#libs[@]} libraries"
  libtool -static -o "$out" "${libs[@]}"
  slice_archives+=("$out")
done

# --- Wrap the merged archives into one static-library xcframework ----------------------------
log "creating VelmuxFFmpeg.xcframework"
rm -rf "$DEST_ROOT/VelmuxFFmpeg.xcframework"
xcodebuild -create-xcframework \
  -library "${slice_archives[0]}" \
  -library "${slice_archives[1]}" \
  -output "$DEST_ROOT/VelmuxFFmpeg.xcframework" >/dev/null

# --- Public headers (identical across slices; taken from the device build) --------------------
log "copying public headers"
rm -rf "$DEST_ROOT/include"
cp -R "$IOS_ROOT/iphoneos/include" "$DEST_ROOT/include"

log "packaged: $DEST_ROOT/VelmuxFFmpeg.xcframework"
