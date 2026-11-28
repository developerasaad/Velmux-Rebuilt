#!/usr/bin/env python3
"""Exercise Apple wrapper profiles with stub tools, including macOS Bash 3.2."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile

source = Path(__file__).resolve().parent
with tempfile.TemporaryDirectory(prefix="velmux-apple-wrapper-") as temporary:
    root = Path(temporary)
    interop = root / "scripts/interop"
    interop.mkdir(parents=True)
    shutil.copy2(source / "build-apple.sh", interop / "build-apple.sh")
    shutil.copy2(source / "link-flags.py", interop / "link-flags.py")
    (root / "scripts/ffmpeg").mkdir()
    (root / "scripts/check-build-environment.sh").write_text("#!/bin/sh\nexit 0\n")
    (root / "scripts/ffmpeg/identity.sh").write_text("#!/bin/sh\necho fixture-identity\n")
    (interop / "generate-swift.sh").write_text('''#!/bin/sh
set -eu
mkdir -p "$2"
for file in VelmuxCore.swift VelmuxCoreFFI.h VelmuxCoreFFI.modulemap; do
    printf 'generated-fixture\\n' > "$2/$file"
done
''')
    (interop / "apple-identity.py").write_text('''from pathlib import Path
import sys
Path(sys.argv[2], "TARGET-IDENTITY").write_text("fixture-identity\\n")
''')
    tools = root / "tools"
    tools.mkdir()
    cargo = tools / "cargo"
    cargo.write_text('''#!/usr/bin/env python3
import json
from pathlib import Path
import sys
args = sys.argv[1:]
target = args[args.index("--target") + 1]
profile = "release" if "--release" in args else "debug"
archive = Path("core/target", target, profile, "libvelmux_core.a")
archive.parent.mkdir(parents=True, exist_ok=True)
archive.write_bytes(b"archive-fixture")
Path("cargo-args.json").write_text(json.dumps(args))
print("note: native-static-libs: -lSystem -framework Security")
''')
    cargo.chmod(0o755)
    xcrun = tools / "xcrun"
    xcrun.write_text("#!/bin/sh\necho fixture-archive-inspection\n")
    xcrun.chmod(0o755)
    environment = os.environ.copy()
    environment["PATH"] = str(tools) + os.pathsep + environment["PATH"]
    for sdk, configuration, target in (
        ("iphoneos", "Release", "aarch64-apple-ios"),
        ("iphonesimulator", "Debug", "aarch64-apple-ios-sim"),
    ):
        prefix = root / "scripts/ffmpeg-builder/out/ios" / sdk
        prefix.mkdir(parents=True)
        (prefix / "BUILD-IDENTITY").write_text("fixture-identity\n")
        result = subprocess.run(
            ["/bin/bash", str(interop / "build-apple.sh"), sdk, configuration],
            cwd=root, env=environment, text=True, capture_output=True,
        )
        assert result.returncode == 0, result.stdout + result.stderr
        args = json.loads((root / "cargo-args.json").read_text())
        assert args[:2] == ["rustc", "--locked"], args
        assert args[args.index("--target") + 1] == target, args
        assert ("--release" in args) == (configuration == "Release"), args
        assert args[-2:] == ["--", "--print=native-static-libs"], args
        assert "" not in args, args
        output = root / "build/ios" / sdk / configuration
        assert (output / "libvelmux_core.a").read_bytes() == b"archive-fixture"
        assert (output / "TARGET-IDENTITY").read_text() == "fixture-identity\n"
        flags = json.loads((output / "native-link-flags.json").read_text())
        assert flags[:3] == ["-lSystem", "-framework", "Security"], flags
        assert "-lc++" in flags and "-liconv" in flags and "-lz" in flags, flags
print("Verified Apple wrapper Release/device and Debug/simulator argument and artifact contracts.")
