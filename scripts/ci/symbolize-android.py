#!/usr/bin/env python3
"""Resolve API 29 APK-relative crash PCs against matching native debug libraries."""
import os
from pathlib import Path
import re
import struct
import subprocess
import zipfile

apk_path = Path('platforms/android/build/outputs/apk/debug/android-debug.apk')
log = Path('build/android-logcat.txt').read_text(errors='replace')
pcs = {int(value, 16) for value in re.findall(r'#\d+ pc ([0-9a-f]+).*com\.velmux\.mobile.*base\.apk', log)}
ndk = Path(os.environ.get('ANDROID_NDK_HOME', os.environ.get('ANDROID_NDK_ROOT', '')))
addr2line = next(ndk.glob('toolchains/llvm/prebuilt/*/bin/llvm-addr2line'))
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
