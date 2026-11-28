#!/usr/bin/env python3
"""Keep the actual Rust staticlib system closure when linking from Swift."""
import json
from pathlib import Path
import re
import shlex
import sys

log, destination = map(Path, sys.argv[1:3])
matches = re.findall(r"native-static-libs:\s*([^\n\r]+)", log.read_text())
if not matches:
    raise SystemExit("Rust did not report its required native static-library closure")
flags = shlex.split(matches[-1])
# The prior Apple host explicitly supplies these non-bundled dependencies.
# AVFoundation is also used by this proof's independent native AAC decoder.
for library in ("-lc++", "-liconv", "-lz"):
    if library not in flags:
        flags.append(library)
for framework in ("Foundation", "AVFoundation", "VideoToolbox", "CoreMedia", "CoreVideo", "AudioToolbox"):
    if not any(flags[i:i + 2] == ["-framework", framework] for i in range(len(flags))):
        flags.extend(["-framework", framework])
if any("\n" in flag or "\r" in flag for flag in flags):
    raise SystemExit("Invalid native linker argument")
destination.write_text(json.dumps(flags, indent=2) + "\n")
