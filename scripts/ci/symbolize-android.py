#!/usr/bin/env python3
"""Resolve API 29 APK-relative crash PCs against matching native debug libraries."""
import os
from pathlib import Path
import re
import struct
import subprocess
import sys
import zipfile

ndk = Path(os.environ.get('ANDROID_NDK_HOME', os.environ.get('ANDROID_NDK_ROOT', '')))
addr2line = next(ndk.glob('toolchains/llvm/prebuilt/*/bin/llvm-addr2line'))
if len(sys.argv) > 1:
    addresses = [hex(int(value, 16)) for value in sys.argv[1].split()]
    library = Path('core/target/x86_64-linux-android/debug/libvelmux_core.so')
    subprocess.run([str(addr2line), '-f', '-C', '-e', str(library), *addresses], check=True)
    readelf = addr2line.with_name('llvm-readelf')
    subprocess.run([str(readelf), '-d', str(library)], check=True)
    outputs = sorted(Path('core/target/x86_64-linux-android/debug/build').glob('velmux_core-*/output'), key=lambda path: path.stat().st_mtime)
    if outputs:
        print("Emitted Cargo native linkage:", flush=True)
        for line in outputs[-1].read_text().splitlines():
            if line.startswith('cargo:rustc-link'):
                print(line, flush=True)
    # A tiny independent link distinguishes SDK search-path behavior from
    # Rust archive metadata without rebuilding FFmpeg or launching an emulator.
    directory = Path('build/android-link-probes')
    directory.mkdir(parents=True, exist_ok=True)
    source = directory / 'platform.c'
    source.write_text('#include <stdio.h>\nint probe(void) { return puts("link probe"); }\n')
    compiler = addr2line.with_name('x86_64-linux-android29-clang')
    base = addr2line.parent.parent / 'sysroot/usr/lib/x86_64-linux-android'
    print(f"Base SDK libraries: {list(base.glob('libc.*'))}", flush=True)
    for name, paths in [('default', []), ('base-first', [base]), ('api-first', [base / '29', base])]:
        output = directory / (name + '.so')
        command = [str(compiler), '-shared', '-fPIC', '-Wl,-t', *['-L' + str(path) for path in paths], str(source), '-o', str(output)]
        print(f"Platform link probe: {name}", flush=True)
        result = subprocess.run(command, check=False)
        if result.returncode == 0:
            subprocess.run([str(readelf), '-d', str(output)], check=True)
    raise SystemExit(0)

apk_path = Path('platforms/android/build/outputs/apk/debug/android-debug.apk')
log = Path('build/android-logcat.txt').read_text(errors='replace')
pcs = {int(value, 16) for value in re.findall(r'#\d+ pc ([0-9a-f]+).*com\.velmux\.mobile.*base\.apk', log)}
# Android 10 sometimes reports a PC relative to the executable map, not the
# complete ELF or APK. Resolve its absolute RIP through the retained map first.
for tombstone in Path('build/android-tombstones').glob('tombstone_*'):
    text = tombstone.read_text(errors='replace')
    match = re.search(r'\brip ([0-9a-f]+)', text)
    if not match:
        continue
    instruction = int(match.group(1), 16)
    for line in text.splitlines():
        mapping = re.search(r"([0-9a-f']+)-([0-9a-f']+)\s+r-x\s+([0-9a-f]+).*com\.velmux\.mobile.*base\.apk", line)
        if mapping:
            start, end, file_offset = [int(value.replace("'", ''), 16) for value in mapping.groups()]
            if start <= instruction <= end:
                pcs.add(file_offset + instruction - start)
with zipfile.ZipFile(apk_path) as apk:
    for entry in apk.infolist():
        if not entry.filename.startswith('lib/x86_64/') or not entry.filename.endswith('.so'):
            continue
        apk.fp.seek(entry.header_offset)
        header = apk.fp.read(30)
        name_length, extra_length = struct.unpack_from('<HH', header, 26)
        data_offset = entry.header_offset + 30 + name_length + extra_length
        for pc in sorted(pcs):
            if not data_offset <= pc < data_offset + entry.file_size:
                continue
            offset = pc - data_offset
            data = apk.read(entry)
            phoff = struct.unpack_from('<Q', data, 32)[0]
            phentsize, phnum = struct.unpack_from('<HH', data, 54)
            for index in range(phnum):
                segment = phoff + index * phentsize
                kind = struct.unpack_from('<I', data, segment)[0]
                file_offset, virtual_address, _, file_size = struct.unpack_from('<QQQQ', data, segment + 8)
                if kind == 1 and file_offset <= offset < file_offset + file_size:
                    address = virtual_address + offset - file_offset
                    break
            else:
                address = offset
            library = Path('core/target/x86_64-linux-android/debug') / Path(entry.filename).name
            if not library.exists():
                library = Path('build/android-symbol-libraries') / Path(entry.filename).name
                library.parent.mkdir(parents=True, exist_ok=True)
                library.write_bytes(data)
            print(f'APK PC {pc:#x}: {entry.filename}, ELF address {address:#x}', flush=True)
            subprocess.run([str(addr2line), '-f', '-C', '-e', str(library), hex(address)], check=True)
if not pcs:
    print('No APK-relative app crash PC in logcat; inspect the retained tombstones.')
