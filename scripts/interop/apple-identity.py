#!/usr/bin/env python3
"""Describe exact direct Swift artifacts without creating a competing cache."""
import hashlib
import json
from pathlib import Path
import subprocess
import sys

root, destination = (Path(value).resolve() for value in sys.argv[1:3])
sdk, target, configuration = sys.argv[3:6]
prefix = root / "scripts/ffmpeg-builder/out/ios" / sdk


def digest(path):
    value = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            value.update(block)
    return value.hexdigest()


def command(*arguments):
    return subprocess.check_output(arguments, text=True).strip()


inputs = [root / "core/Cargo.toml", root / "core/Cargo.lock", root / "core/build.rs",
          root / "rust-toolchain.toml"]
inputs.extend(path for path in (root / "core/src").rglob("*") if path.is_file())
inputs.extend(path for path in (root / "scripts/interop").rglob("*") if path.is_file())
record = prefix / "BUILD-IDENTITY"
manifest = prefix / "share/velmux/ffmpeg-capabilities.json"
content = {
    "schema": 1,
    "target": target,
    "sdk": sdk,
    "configuration": configuration,
    "ios_minimum": "15.0",
    "source_sha256": {str(path.relative_to(root)): digest(path) for path in sorted(set(inputs))},
    "native_recipe_identity": record.read_text().splitlines()[0],
    "native_build_record_sha256": digest(record),
    "native_capabilities_sha256": digest(manifest),
    "rustc": command("rustc", "-vV"),
    "xcode": command("xcodebuild", "-version"),
    "sdk_version": command("xcrun", "--sdk", sdk, "--show-sdk-version"),
    "sdk_build": command("xcrun", "--sdk", sdk, "--show-sdk-build-version"),
    "sdk_path": command("xcrun", "--sdk", sdk, "--show-sdk-path"),
    "output_sha256": {
        name: digest(destination / name) for name in (
            "libvelmux_core.a", "generated/VelmuxCore.swift", "generated/VelmuxCoreFFI.h",
            "generated/VelmuxCoreFFI.modulemap", "native-link-flags.json",
        )
    },
}
canonical = json.dumps(content, sort_keys=True, separators=(",", ":")).encode()
content["content_identity"] = hashlib.sha256(canonical).hexdigest()
(destination / "TARGET-IDENTITY").write_text(json.dumps(content, sort_keys=True, indent=2) + "\n")
