#!/usr/bin/env python3
"""Inspect the actual APK native closure, including ELF target and page alignment."""
import struct
import sys
import zipfile

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
            phoff = struct.unpack_from("<Q", data, 32)[0]
            phentsize, phnum = struct.unpack_from("<HH", data, 54)
            for index in range(phnum):
                offset = phoff + index * phentsize
                if struct.unpack_from("<I", data, offset)[0] == 1:
                    alignment = struct.unpack_from("<Q", data, offset + 48)[0]
                    assert alignment >= 16384, f"{path}: LOAD alignment {alignment} below 16 KiB"
        print(f"Native APK closure verified: {abi}")
