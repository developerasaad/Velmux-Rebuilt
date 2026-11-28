#!/usr/bin/env bash
# Reusable target core + generated Swift output; this never builds a UI or a
# macOS FFmpeg host merely to inspect target-archive metadata.
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
cd "$root"
bash scripts/check-build-environment.sh
sdk=${1:?Apple SDK required}
configuration=${2:?Debug or Release required}
case "$sdk" in
  iphoneos) target=aarch64-apple-ios; slice=device ;;
  iphonesimulator) target=aarch64-apple-ios-sim; slice=simulator ;;
  *) echo 'Expected iphoneos or iphonesimulator.' >&2; exit 2 ;;
esac
case "$configuration" in
  Debug) profile=debug ;;
  Release) profile=release ;;
  *) echo 'Expected Debug or Release.' >&2; exit 2 ;;
esac
destination="$root/build/ios/$sdk/$configuration"
mkdir -p "$destination"
expected_native=$(bash scripts/ffmpeg/identity.sh ios "$slice")
prefix="$root/scripts/ffmpeg-builder/out/ios/$sdk"
[ -s "$prefix/BUILD-IDENTITY" ] && [ "$(sed -n '1p' "$prefix/BUILD-IDENTITY")" = "$expected_native" ] || {
  echo 'Restore or construct the current identity-matching Apple dependencies first.' >&2; exit 1;
}
export IPHONEOS_DEPLOYMENT_TARGET=15.0
export CARGO_TERM_COLOR=never
# core/build.rs selects and validates the existing managed Apple FFmpeg prefix.
# Native archives are bundled into libvelmux_core.a; SDK/system libraries remain
# external and their actual linker requirements are captured for Swift/Xcode.
# macOS ships Bash 3.2, where an empty array is unbound under nounset.
# Keep the complete argument vector nonempty in both configurations.
cargo_args=(rustc --locked --manifest-path core/Cargo.toml --lib --target "$target")
if [ "$configuration" = Release ]; then cargo_args+=(--release); fi
cargo "${cargo_args[@]}" -- --print=native-static-libs 2>&1 | tee "$destination/rust-build.log"
archive="$root/core/target/$target/$profile/libvelmux_core.a"
[ -s "$archive" ] || { echo 'The Apple Rust staticlib was not produced.' >&2; exit 1; }
cp "$archive" "$destination/libvelmux_core.a"
bash scripts/interop/generate-swift.sh "$destination/libvelmux_core.a" "$destination/generated"
python3 scripts/interop/link-flags.py "$destination/rust-build.log" "$destination/native-link-flags.json"
xcrun lipo -info "$destination/libvelmux_core.a" > "$destination/archive-architecture.txt"
xcrun ar -t "$destination/libvelmux_core.a" > "$destination/archive-members.txt"
shasum -a 256 "$destination/libvelmux_core.a" "$destination/generated/VelmuxCore.swift" \
  "$destination/generated/VelmuxCoreFFI.h" "$destination/generated/VelmuxCoreFFI.modulemap" > "$destination/artifact-sha256.txt"
python3 scripts/interop/apple-identity.py "$root" "$destination" "$sdk" "$target" "$configuration"
