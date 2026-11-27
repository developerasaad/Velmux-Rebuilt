#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/../.."
python3 - <<'CHECK'
import ast, pathlib
for path in pathlib.Path('scripts').rglob('*.py'):
    if not any(part in {'out', 'src', 'downloads'} for part in path.relative_to('scripts').parts):
        ast.parse(path.read_text(), filename=str(path))
CHECK
while IFS= read -r script; do bash -n "$script"; done < <(git ls-files '*.sh')
python3 tests/build/test_native_identity.py
cargo fmt --manifest-path core/Cargo.toml -- --check
git diff --check
