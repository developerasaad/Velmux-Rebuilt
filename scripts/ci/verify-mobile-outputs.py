#!/usr/bin/env python3
"""Independently decode published outputs retained from real native test flows."""
import hashlib
import importlib.util
import json
from pathlib import Path
import re
import shutil
import sys
import tarfile


ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("media_verification", Path(__file__).with_name("verify-media-outputs.py"))
media = importlib.util.module_from_spec(spec)
spec.loader.exec_module(media)


def digest(path):
    result = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(128 * 1024), b""):
            result.update(block)
    return result.hexdigest()


def retained_copy(source, target):
    if source.is_symlink() or not source.is_file():
        raise AssertionError(f"A published test output is missing or is a symbolic link: {source}")
    target.parent.mkdir(parents=True, exist_ok=True)
    try:
        with source.open("rb") as original, target.open("xb") as evidence:
            shutil.copyfileobj(original, evidence, 128 * 1024)
    except FileExistsError:
        assert not target.is_symlink() and digest(target) == digest(source), f"Unidentified evidence file preserved: {target}"
    assert digest(target) == digest(source), f"Evidence does not match the published output: {source}"
    return target


def video_case(path):
    return {"path": str(path), "container": "mp4", "video_codec": "h264", "width": 320, "height": 180,
            "audio_codecs": ["aac"], "sample_rate": 48000, "channels": 2, "duration": 2.0,
            "frame_rate": 24000 / 1001, "tones": True, "source": "video.mp4",
            "preserve_video_timestamps": True, "reencoded_video": True, "decoded_video_frames": 48,
            "match_source_frame": True}


def audio_case(path, codec):
    return {"path": str(path), "container": "flac" if codec == "flac" else "mp4", "audio_codecs": [codec], "sample_rate": 48000,
            "channels": 2, "duration": 2.0, "tones": True}


def main():
    platform, location = sys.argv[1], Path(sys.argv[2]).resolve()
    if platform == "ios":
        published = location / "Documents/Velmux"
        destination = ROOT / "build/ios-screenshots/media"
        outputs = {}
        for operation, extension in [("video", "mp4"), ("audio", "m4a")]:
            pattern = re.compile(rf"picker-{operation}-[0-9a-f]{{8}}-(?:[0-9a-f]{{4}}-){{3}}[0-9a-f]{{12}}\.{extension}")
            matches = [path for path in published.iterdir() if pattern.fullmatch(path.name)]
            assert len(matches) == 1, f"Expected one actual picker {operation} publication, found {matches}"
            outputs[operation] = retained_copy(matches[0], destination / matches[0].name)
        cases = [video_case(outputs["video"]), audio_case(outputs["audio"], "aac")]
    elif platform == "android":
        api = sys.argv[3]
        assert api.isdecimal(), "An actual Android API level is required"
        destination = location.parent / "media"
        destination.mkdir(parents=True, exist_ok=True)
        outputs = {}
        with tarfile.open(location) as archive:
            for name in ["android-video-converted.mp4", "android-audio.m4a", "android-audio.flac"]:
                member_name = f"evidence/api-{api}-{name}"
                members = [member for member in archive.getmembers() if member.name == member_name]
                assert len(members) == 1 and members[0].isfile(), f"Missing regular output evidence: {member_name}"
                assert 0 < members[0].size <= 64 * 1024 * 1024, f"Unreasonable fixture output size: {member_name}"
                data = archive.extractfile(members[0]).read(64 * 1024 * 1024 + 1)
                assert len(data) == members[0].size, f"Truncated output evidence: {member_name}"
                target = destination / Path(member_name).name
                try:
                    with target.open("xb") as evidence:
                        evidence.write(data)
                except FileExistsError:
                    assert not target.is_symlink() and digest(target) == hashlib.sha256(data).hexdigest(), f"Unidentified evidence preserved: {target}"
                outputs[name] = target
        cases = [video_case(outputs["android-video-converted.mp4"]),
                 audio_case(outputs["android-audio.m4a"], "aac"),
                 audio_case(outputs["android-audio.flac"], "flac")]
    else:
        raise AssertionError(f"Unknown verification platform: {platform}")
    media.OUTPUTS = destination
    fixtures = json.loads((media.FIXTURES / "fixtures.json").read_text())["files"]
    for name in ["video.mp4", "stereo.wav"]:
        assert digest(media.FIXTURES / name) == fixtures[name], f"Original fixture changed: {name}"
    verified = [media.check_case(case) for case in cases]
    if platform == "android":
        def pcm(path):
            return media.run(["ffmpeg", "-v", "error", "-nostdin", "-i", str(path), "-map", "0:a:0",
                              "-f", "s16le", "-acodec", "pcm_s16le", "-"])
        assert pcm(outputs["android-audio.flac"]) == pcm(media.FIXTURES / "stereo.wav"), "Published FLAC changed original decoded PCM"
    result = {"platform": platform, "verified_outputs": verified,
              "output_sha256": {case["path"]: digest(Path(case["path"])) for case in cases},
              "independent_tool": media.run(["ffprobe", "-version"]).decode().splitlines()[0],
              "source_hashes_unchanged": True}
    (destination / "independent-mobile-verification.json").write_text(json.dumps(result, indent=2) + "\n")
    print(f"Independently decoded and verified {len(cases)} published {platform} outputs, including real picker video encoding")


if __name__ == "__main__":
    main()
