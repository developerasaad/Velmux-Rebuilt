#!/usr/bin/env bash
# Native caches describe the repository recipes and selected toolchains.
# Reject inherited compiler overrides rather than reuse an incorrectly identified build.
set -euo pipefail
for variable in CC CXX CFLAGS CXXFLAGS CPPFLAGS LDFLAGS; do
  if [ -n "${!variable:-}" ]; then
    printf 'Unsupported inherited %s; put native compiler changes in the pinned build recipe.\n' "$variable" >&2
    exit 1
  fi
done
if [ "${1:-}" = android ] && [ "${BASH_VERSINFO[0]}" -lt 5 ]; then
  echo 'Android build commands require Bash 5 or newer.' >&2
  exit 1
fi
