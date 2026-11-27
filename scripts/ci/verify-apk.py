#!/usr/bin/env python3
"""Inspect the actual APK native closure, including ELF target and page alignment."""
import os
from pathlib import Path
import subprocess
import struct
import sys
import zipfile

def needed_libraries(data):
    phoff = struct.unpack_from("<Q", data, 32)[0]
    phentsize, phnum = struct.unpack_from("<HH", data, 54)
    segments = [struct.unpack_from("<IIQQQQQQ", data, phoff + i * phentsize) for i in range(phnum)]
    dynamic = next(segment for segment in segments if segment[0] == 2)
    entries = [struct.unpack_from("<QQ", data, offset) for offset in range(dynamic[2], dynamic[2] + dynamic[5], 16)]
    string_address = next(value for kind, value in entries if kind == 5)
    load = next(segment for segment in segments if segment[0] == 1 and segment[3] <= string_address < segment[3] + segment[5])
    strings = load[2] + string_address - load[3]
    return [data[strings + value:data.index(b"\0", strings + value)].decode() for kind, value in entries if kind == 1]

machines = {"arm64-v8a": 183, "x86_64": 62}
with zipfile.ZipFile(sys.argv[1]) as apk:
    for abi, machine in machines.items():
        for library in ("libvelmux_core.so", "libc++_shared.so", "libjnidispatch.so"):
            path = f"lib/{abi}/{library}"
            entry = apk.getinfo(path)
            if entry.compress_type == zipfile.ZIP_STORED:
                apk.fp.seek(entry.header_offset)
                header = apk.fp.read(30)
                name_length, extra_length = struct.unpack_from("<HH", header, 26)
                data_offset = entry.header_offset + 30 + name_length + extra_length
                assert data_offset % 16384 == 0, f"{path}: APK data offset is not 16 KiB aligned"
            data = apk.read(path)
            assert data[:5] == b"\x7fELF\x02", path
            assert struct.unpack_from("<H", data, 18)[0] == machine, path
            if library == "libvelmux_core.so":
                needed = needed_libraries(data)
                assert "libc.so" in needed, f"{path}: Android libc must remain a platform dependency"
                assert needed.count("libc++_shared.so") == 1, f"{path}: expected one shared C++ runtime"
                assert "libstdc++.so" not in needed, f"{path}: legacy Android C++ runtime is not supported"
                ndk = Path(os.environ.get("ANDROID_NDK_HOME", os.environ.get("ANDROID_NDK_ROOT", "")))
                nm = next(ndk.glob("toolchains/llvm/prebuilt/*/bin/llvm-nm"))
                triple = {"arm64-v8a": "aarch64-linux-android", "x86_64": "x86_64-linux-android"}[abi]
                unstripped = Path("core/target") / triple / "release/libvelmux_core.so"
                symbols = subprocess.check_output([str(nm), "--defined-only", str(unstripped)], text=True)
                assert "__bionic_getauxval" not in symbols, f"{path}: platform libc was bundled into the engine"
                print(f"Platform dynamic dependencies: {abi}: {', '.join(needed)}")
            phoff = struct.unpack_from("<Q", data, 32)[0]
            phentsize, phnum = struct.unpack_from("<HH", data, 54)
            for index in range(phnum):
                offset = phoff + index * phentsize
                if struct.unpack_from("<I", data, offset)[0] == 1:
                    alignment = struct.unpack_from("<Q", data, offset + 48)[0]
                    assert alignment >= 16384, f"{path}: LOAD alignment {alignment} below 16 KiB"
        print(f"Native APK closure verified: {abi}")
