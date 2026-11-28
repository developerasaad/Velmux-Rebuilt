#!/usr/bin/env python3
"""Exercise native identity isolation using temporary recipe copies and tool stubs."""
import json
import os
import pathlib
import shutil
import subprocess
import tempfile

repo = pathlib.Path(__file__).resolve().parents[2]
tracked = subprocess.check_output(
    ['git', 'ls-files', 'scripts/ffmpeg-builder', 'scripts/ffmpeg', 'scripts/lib'],
    cwd=repo,
    text=True,
).splitlines()
with tempfile.TemporaryDirectory(prefix='velmux-identity-direct-') as folder:
    root = pathlib.Path(folder)
    recipe = root / 'repo'
    tools = root / 'tools'
    tools.mkdir()
    systemlib = root / 'systemlib'
    systemlib.mkdir()
    (systemlib / 'libz.a').write_bytes(b'zlib-fixture')
    for filename in tracked:
        destination = recipe / filename
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(repo / filename, destination)
    for name in [
        'cmake', 'ninja', 'meson', 'nasm', 'autoconf', 'automake',
        'libtoolize', 'glibtoolize', 'xcodebuild', 'xcrun', 'cc', 'c++', 'gcc',
    ]:
        tool = tools / name
        tool.write_text('#!/bin/sh\necho external-' + name + '-1\n')
        tool.chmod(0o755)
    tool = tools / 'pkg-config'
    tool.write_text(
        '#!/bin/sh\ncase "$1" in\n'
        ' --modversion) echo 1.3.1;;\n'
        ' --variable=libdir) echo "' + str(systemlib) + '";;\n'
        ' *) echo external-pkg-config-1;;\nesac\n'
    )
    tool.chmod(0o755)
    ndk = root / 'ndk'
    ndk.mkdir()
    (ndk / 'source.properties').write_text('Pkg.Revision = 27.2.12479018\n')
    environment = os.environ.copy()
    environment.update(
        PATH=str(tools) + os.pathsep + environment['PATH'],
        CC='cc',
        CXX='c++',
        ANDROID_NDK_HOME=str(ndk),
        VELMUX_OUT_DIR=str(root / 'out'),
        VELMUX_SRC_DIR=str(root / 'src'),
    )
    policies = {
        'host': [],
        'android': ['abi=arm64-v8a', 'api=29', 'svt=1'],
        'ios': ['slice=device-arm64', 'deployment=15.0', 'svt=1'],
    }

    def hashes(active_environment=environment):
        result_hashes = {}
        for name, policy in policies.items():
            result = subprocess.run(
                [
                    'bash', '-c', '. "$1/lib.sh"; shift; compute_identity "$@"',
                    'fixture', str(recipe / 'scripts/ffmpeg-builder'), name, *policy,
                ],
                cwd=recipe,
                env=active_environment,
                text=True,
                capture_output=True,
                check=True,
            )
            assert not result.stderr, result.stderr
            value = result.stdout.strip()
            assert len(value) == 64 and all(c in '0123456789abcdef' for c in value)
            result_hashes[name] = value
        return result_hashes

    baseline = hashes()
    checks = []
    mutations = [
        ('scripts/ffmpeg-builder/build-ios.sh', {'ios'}),
        ('scripts/ffmpeg-builder/lib.sh', set(policies)),
        ('scripts/ffmpeg-builder/patch-aiff-layouts.py', set(policies)),
        ('scripts/lib/common.sh', set(policies)),
        ('scripts/ffmpeg-builder/versions.json', set(policies)),
        ('scripts/ffmpeg-builder/probe/velmux-ffmpeg-probe.c', {'host'}),
        ('scripts/ffmpeg-builder/manifest-from-config.py', {'android', 'ios'}),
        ('scripts/ffmpeg-builder/package-ios-xcframework.sh', {'ios'}),
    ]
    for filename, expected in mutations:
        path = recipe / filename
        contents = path.read_bytes()
        path.write_bytes(contents + b'\n# change sensitivity fixture\n')
        actual = hashes()
        path.write_bytes(contents)
        changed = {name for name in baseline if actual[name] != baseline[name]}
        assert changed == expected, (filename, changed, expected)
        checks.append([filename, sorted(changed)])

    generated_paths = []
    for tree in ['out/tools/bin', 'src/cross/bin']:
        directory = root / tree
        directory.mkdir(parents=True)
        generated_paths.append(str(directory))
        for name in [
            'cmake', 'ninja', 'meson', 'pkg-config', 'nasm', 'autoconf',
            'automake', 'libtoolize', 'glibtoolize', 'xcodebuild', 'xcrun', 'cc', 'c++',
        ]:
            tool = directory / name
            tool.write_text('#!/bin/sh\necho WRONG-generated-tool\n')
            tool.chmod(0o755)
    altered_environment = environment.copy()
    altered_environment['PATH'] = os.pathsep.join(generated_paths + [environment['PATH']])
    assert hashes(altered_environment) == baseline
    checks.append(['generated OUT/SRC PATH', []])

    (tools / 'meson').write_text('#!/bin/sh\necho external-meson-2\n')
    actual = hashes()
    assert all(actual[name] != baseline[name] for name in policies)
    checks.append(['external Meson', sorted(policies)])
    print(json.dumps({
        'source_commit': subprocess.check_output(
            ['git', 'rev-parse', 'HEAD'], cwd=repo, text=True,
        ).strip(),
        'verification': 'direct compute_identity; explicit policy; stderr required empty',
        'checks': checks,
        'result': 'all 10 checks passed',
    }, indent=2))
