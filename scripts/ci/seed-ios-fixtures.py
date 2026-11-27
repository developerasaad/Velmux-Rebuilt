#!/usr/bin/env python3
"""Seed only known generated files, then independently verify their preservation."""
import hashlib
import json
from pathlib import Path
import sys


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


if sys.argv[1] == "--verify":
    manifest = json.loads(Path(sys.argv[2]).read_text())
    for name, expected in manifest.items():
        path = Path(name)
        if path.is_symlink() or digest(path) != expected:
            raise SystemExit(f"The selected original was modified: {path}")
    print("Independent iOS source hashes unchanged")
else:
    container, fixtures, evidence = map(Path, sys.argv[1:4])
    directory = container / "Documents/UITestSources"
    directory.mkdir(parents=True, exist_ok=True)
    if directory.is_symlink():
        raise SystemExit("The fixture source directory is not an owned directory")
    manifest = {}
    # The generic-data alias proves actual-content inspection through the Files
    # picker, rather than depending on provider/native media type recognition.
    for name, original in (
        ("video.mp4", "video.mp4"),
        ("video.bin", "video.mp4"),
        ("stereo.wav", "stereo.wav"),
    ):
        source, target = fixtures / original, directory / name
        data = source.read_bytes()
        expected = hashlib.sha256(data).hexdigest()
        try:
            with target.open("xb") as output:
                output.write(data)
        except FileExistsError:
            if target.is_symlink() or digest(target) != expected:
                raise SystemExit(f"An unidentified existing source was preserved: {target}")
        manifest[str(target)] = expected
    evidence.write_text(json.dumps(manifest, indent=2) + "\n")
