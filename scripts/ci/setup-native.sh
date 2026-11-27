#!/usr/bin/env bash
# Run only on disposable CI workers; never alter a developer workstation.
set -euo pipefail
[ "${CI:-}" = "true" ] || { echo "This setup script requires a CI worker" >&2; exit 1; }
bash scripts/check-build-environment.sh
case "$(uname -s)" in
  Linux)
    sudo apt-get update -qq
    sudo apt-get install -y autoconf automake libtool pkg-config nasm libclang-dev
    ;;
  Darwin)
    # Brew supplies host-only build tools; target dependencies remain source-pinned.
    for tool in autoconf automake libtool pkg-config nasm; do
      brew list "$tool" >/dev/null 2>&1 || brew install "$tool"
    done
    ;;
esac
python3 -m venv build/ci-tools
build/ci-tools/bin/pip install --disable-pip-version-check cmake==3.31.6 ninja==1.11.1.3 meson==1.7.2
printf '%s/build/ci-tools/bin\n' "$PWD" >> "$GITHUB_PATH"
