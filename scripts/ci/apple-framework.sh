#!/usr/bin/env bash
# Build the shared framework, or reuse an exact CI source/toolchain identity.
set -euo pipefail
cd "$(dirname "$0")/../.."
sdk=${1:?SDK required}
variant=${2:?Variant required}
case "$sdk" in
  iphoneos) target=IosArm64; directory=iosArm64 ;;
  iphonesimulator) target=IosSimulatorArm64; directory=iosSimulatorArm64 ;;
  *) echo 'Unsupported Apple SDK' >&2; exit 2 ;;
esac
case "$variant" in
  Debug) folder=debugFramework ;;
  Release) folder=releaseFramework ;;
  *) echo 'Unsupported framework variant' >&2; exit 2 ;;
esac
destination="build/ios/$sdk"
identity="${VELMUX_FRAMEWORK_ID:-}-$sdk-$variant"
if [ "${CI:-}" = true ] && [ -n "${VELMUX_FRAMEWORK_ID:-}" ] &&
   [ -s "$destination/VelmuxUI.framework/VelmuxUI" ] &&
   [ -s "$destination/VelmuxUI.framework/Headers/VelmuxUI.h" ] &&
   [ -f "$destination/FRAMEWORK-IDENTITY" ] &&
   [ "$(cat "$destination/FRAMEWORK-IDENTITY")" = "$identity" ]; then
  echo "Reusing verified-identity shared framework: $sdk $variant"
  exit 0
fi
./gradlew -PbindingsTarget=ios ":UI:link${variant}Framework$target"
mkdir -p "$destination"
rm -rf "$destination/VelmuxUI.framework"
cp -R "UI/build/bin/$directory/$folder/VelmuxUI.framework" "$destination/"
printf '%s\n' "$identity" > "$destination/FRAMEWORK-IDENTITY"
