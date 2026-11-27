#!/usr/bin/env bash
# Build (or identity-verified reuse of) the iOS FFmpeg artifacts: device
# arm64 + simulator arm64 slices and the xcframework inputs. macOS only.
#
# Contract: scripts/ffmpeg/build-ios.sh
set -euo pipefail

SCRIPTS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=lib/common.sh
. "$SCRIPTS_DIR/lib/common.sh"

case "$(uname -s)" in
  Darwin) ;;
  *) vdie "the iOS FFmpeg artifacts can only be built on macOS (Xcode required)" ;;
esac

bash "$SCRIPTS_DIR/ffmpeg/fetch.sh" ios
