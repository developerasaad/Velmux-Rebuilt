#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/../.."
bash scripts/ffmpeg/fetch.sh host
bash scripts/ci/prepare-media-fixtures.sh
cargo test --locked --manifest-path core/Cargo.toml
python3 scripts/ci/verify-media-outputs.py
cargo clippy --locked --manifest-path core/Cargo.toml --all-targets -- -D warnings
