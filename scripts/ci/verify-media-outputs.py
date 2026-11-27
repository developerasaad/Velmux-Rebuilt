#!/usr/bin/env python3
"""Inspect and decode real core outputs with independently installed FFmpeg tools."""
import array
import hashlib
import json
import math
from pathlib import Path
import subprocess
import sys
from fractions import Fraction

ROOT = Path(__file__).resolve().parents[2]
FIXTURES = ROOT / "build/media-fixtures"
OUTPUTS = ROOT / "build/media-test-output"


def run(arguments):
    return subprocess.check_output(arguments, stderr=subprocess.PIPE)


def probe(path, packets=False):
    command = ["ffprobe", "-v", "error", "-show_streams", "-show_format"]
    if packets:
        command += ["-show_packets", "-show_data_hash", "sha256"]
    return json.loads(run(command + ["-of", "json", str(path)]))


def media_packets(info, kind):
    indices = {stream["index"] for stream in info["streams"] if stream["codec_type"] == kind}
    return [packet for packet in info.get("packets", []) if packet["stream_index"] in indices]


def assert_close(actual, expected, tolerance, detail):
    assert math.isfinite(actual) and abs(actual - expected) <= tolerance, f"{detail}: {actual} != {expected} ± {tolerance}"


def check_tones(path, expected_channels):
    raw = run(["ffmpeg", "-v", "error", "-nostdin", "-i", str(path), "-map", "0:a:0",
               "-ar", "48000", "-ac", str(expected_channels), "-f", "f32le", "-acodec", "pcm_f32le", "-"])
    samples = array.array("f")
    samples.frombytes(raw)
    if sys.byteorder != "little":
        samples.byteswap()
    assert len(samples) > 48000 * expected_channels, f"{path}: missing decoded tone samples"
    # Discard priming/tail regions; measure known frequencies from actual samples.
    begin, count = 12000, 24000
    for channel in range(expected_channels):
        values = samples[begin * expected_channels + channel:(begin + count) * expected_channels:expected_channels]
        rms = math.sqrt(sum(value * value for value in values) / len(values))
        assert 0.02 < rms < 0.3, f"{path}: silent/clipped tone channel {channel}: RMS {rms}"
        frequencies = [440, 660] if expected_channels == 1 else [[440], [660]][channel]
        for frequency in frequencies:
            cosine = sum(value * math.cos(2 * math.pi * frequency * i / 48000) for i, value in enumerate(values))
            sine = sum(value * math.sin(2 * math.pi * frequency * i / 48000) for i, value in enumerate(values))
            amplitude = 2 * math.hypot(cosine, sine) / len(values)
            assert amplitude > 0.02, f"{path}: expected {frequency} Hz missing in channel {channel}"


def check_case(case):
    path = Path(case["path"])
    assert path.is_file() and path.stat().st_size > 64, f"Missing/nontrivial output: {path}"
    info = probe(path, packets=True)
    video = [stream for stream in info["streams"] if stream["codec_type"] == "video"]
    audio = [stream for stream in info["streams"] if stream["codec_type"] == "audio"]
    if "video_codec" in case:
        assert len(video) == 1 and video[0]["codec_name"] == case["video_codec"], path
        assert (video[0]["width"], video[0]["height"]) == (case["width"], case["height"]), path
        if "frame_rate" in case:
            assert_close(float(Fraction(video[0]["avg_frame_rate"])), case["frame_rate"], 0.01, f"{path}: frame rate")
            timestamps = sorted(float(packet["pts_time"]) for packet in media_packets(info, "video"))
            for before, after in zip(timestamps, timestamps[1:]):
                assert_close(after - before, 1 / case["frame_rate"], 0.0001, f"{path}: frame presentation interval")
    else:
        assert not video, f"{path}: unexpected video in audio output"
    assert [stream["codec_name"] for stream in audio] == case.get("audio_codecs", []), path
    for stream in audio:
        assert int(stream["sample_rate"]) == case["sample_rate"], path
        assert stream["channels"] == case["channels"], path
    if "duration" in case:
        assert_close(float(info["format"]["duration"]), case["duration"], case.get("duration_tolerance", 0.15), str(path))
    run(["ffmpeg", "-v", "error", "-xerror", "-err_detect", "explode", "-nostdin", "-threads", "1",
         "-i", str(path), "-map", "0:v?", "-map", "0:a?", "-f", "null", "-"])
    if case.get("tones"):
        check_tones(path, case["channels"])
    if case.get("source"):
        source = probe(FIXTURES / case["source"], packets=True)
        if case.get("preserve_video_packets"):
            original = media_packets(source, "video")
            result = media_packets(info, "video")
            assert len(original) == len(result), f"{path}: changed copied frame/packet count"
            assert [packet["data_hash"] for packet in original] == [packet["data_hash"] for packet in result], f"{path}: copied compressed video payload changed"
            shift = float(result[0]["pts_time"]) - float(original[0]["pts_time"])
            for before, after in zip(original, result):
                assert_close(float(after["pts_time"]) - shift, float(before["pts_time"]), 0.002, f"{path}: copied presentation timestamp")
        if case.get("preserve_video_timestamps"):
            original = sorted(float(packet["pts_time"]) for packet in media_packets(source, "video"))
            result = sorted(float(packet["pts_time"]) for packet in media_packets(info, "video"))
            assert len(original) == len(result), f"{path}: Original timing changed frame count"
            # A common timeline-origin shift is valid; cadence and A/V offsets must survive.
            shift = result[0] - original[0]
            for before, after in zip(original, result):
                assert_close(after - shift, before, 0.002, f"{path}: Original encoded presentation timestamp")
        if case.get("preserve_av_offset"):
            def offset(document):
                return min(float(packet["pts_time"]) for packet in media_packets(document, "audio")) - min(float(packet["pts_time"]) for packet in media_packets(document, "video"))
            assert_close(offset(info), offset(source), 0.05, f"{path}: audio/video offset")
    return str(path.relative_to(OUTPUTS))


def main():
    fixture_manifest = json.loads((FIXTURES / "fixtures.json").read_text())
    for name, digest in fixture_manifest["files"].items():
        assert hashlib.sha256((FIXTURES / name).read_bytes()).hexdigest() == digest, f"Original fixture changed: {name}"
    manifests = sorted(OUTPUTS.glob("*/expectations.json"))
    assert manifests, "No independently verifiable engine outputs were produced"
    checked = []
    for manifest in manifests:
        for case in json.loads(manifest.read_text()):
            checked.append(check_case(case))
    assert len(checked) >= 8, f"Incomplete conversion output set: {checked}"
    result = {"verified_outputs": checked, "fixture_tool": fixture_manifest["ffmpeg_version"],
              "verification_tool": run(["ffprobe", "-version"]).decode().splitlines()[0],
              "original_hashes_unchanged": len(fixture_manifest["files"])}
    (OUTPUTS / "independent-verification.json").write_text(json.dumps(result, indent=2) + "\n")
    print(f"Independently inspected and decoded {len(checked)} real outputs; original hashes unchanged")


if __name__ == "__main__":
    main()
