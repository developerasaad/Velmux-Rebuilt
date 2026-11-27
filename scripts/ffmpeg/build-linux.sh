#!/usr/bin/env bash
# Build (or identity-verified reuse of) the host FFmpeg artifacts.
# Development/testing only — Linux is not a shipping Velmux platform.
#
# Contract: scripts/ffmpeg/build-linux.sh
set -euo pipefail

SCRIPTS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=lib/common.sh
. "$SCRIPTS_DIR/lib/common.sh"

bash "$SCRIPTS_DIR/ffmpeg/fetch.sh" host
