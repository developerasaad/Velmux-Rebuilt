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


def check_tones(path, expected_channels, expected_frequencies=None):
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
        if expected_frequencies is not None:
            frequencies = expected_frequencies if expected_channels == 1 else [expected_frequencies[channel]]
        else:
            frequencies = [440, 660] if expected_channels == 1 else [[440], [660]][channel]
        for frequency in frequencies:
            cosine = sum(value * math.cos(2 * math.pi * frequency * i / 48000) for i, value in enumerate(values))
            sine = sum(value * math.sin(2 * math.pi * frequency * i / 48000) for i, value in enumerate(values))
            amplitude = 2 * math.hypot(cosine, sine) / len(values)
            assert amplitude > 0.02, f"{path}: expected {frequency} Hz missing in channel {channel}"


def first_rgb(path, width, height, start=0, tone_map=False):
    filters = []
    if tone_map:
        filters += ["zscale=t=linear:npl=100", "format=gbrpf32le", "zscale=p=bt709",
                    "tonemap=tonemap=hable:desat=0", "zscale=t=bt709:m=bt709:r=limited", "format=yuv420p"]
    filters.append(f"scale={width}:{height}:flags=bicubic")
    return run(["ffmpeg", "-v", "error", "-nostdin", "-threads", "1", "-ss", str(start),
                "-i", str(path), "-map", "0:v:0", "-vf", ",".join(filters),
                "-frames:v", "1", "-pix_fmt", "rgb24", "-f", "rawvideo", "-"])


def rgb_error(actual, expected):
    assert len(actual) == len(expected) and actual, "A selected preview frame was not decoded"
    return math.sqrt(sum((a - b) ** 2 for a, b in zip(actual, expected)) / len(actual))


def check_case(case):
    path = Path(case["path"])
    assert path.is_file() and path.stat().st_size > 64, f"Missing/nontrivial output: {path}"
    info = probe(path, packets=True)
    video = [stream for stream in info["streams"] if stream["codec_type"] == "video"]
    audio = [stream for stream in info["streams"] if stream["codec_type"] == "audio"]
    if "video_codec" in case:
        assert len(video) == 1 and video[0]["codec_name"] == case["video_codec"], path
        assert (video[0]["width"], video[0]["height"]) == (case["width"], case["height"]), path
        if "display_aspect_ratio" in case:
            actual = Fraction(video[0]["display_aspect_ratio"].replace(":", "/"))
            assert_close(float(actual), float(Fraction(case["display_aspect_ratio"])), 0.0001, f"{path}: display aspect ratio")
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
        duration = float(info["format"]["duration"])
        if case.get("gapless_duration"):
            assert len(audio) == 1 and audio[0]["codec_name"] == "mp3", path
            # Low-rate MPEG audio has a sizeable coded priming/tail interval.
            # Require actual skip/discard metadata and exact independently
            # decoded sample count rather than widening a duration tolerance.
            skipped = sum(int(side.get("skip_samples", 0)) + int(side.get("discard_padding", 0))
                          for packet in media_packets(info, "audio") for side in packet.get("side_data_list", [])
                          if side.get("side_data_type") == "Skip Samples")
            rate = int(audio[0]["sample_rate"])
            assert 0 < skipped < rate // 2, f"{path}: missing/unreasonable gapless padding"
            assert_close(duration - skipped / rate, case["duration"], 1 / rate, f"{path}: playable duration")
        else:
            assert_close(duration, case["duration"], case.get("duration_tolerance", 0.15), str(path))
    if "decoded_samples" in case:
        raw = run(["ffmpeg", "-v", "error", "-nostdin", "-i", str(path), "-map", "0:a:0", "-f", "f32le", "-acodec", "pcm_f32le", "-"])
        assert len(raw) == case["decoded_samples"] * case["channels"] * 4, f"{path}: decoded sample count changed"
    if "duration_range" in case:
        lower, upper = case["duration_range"]
        assert lower <= float(info["format"]["duration"]) <= upper, f"{path}: preview is not bounded to its selected interval"
    if "reported_preview_duration_us" in case:
        assert_close(float(info["format"]["duration"]), case["reported_preview_duration_us"] / 1_000_000, 0.15, f"{path}: reported preview duration")
    run(["ffmpeg", "-v", "error", "-xerror", "-err_detect", "explode", "-nostdin", "-threads", "1",
         "-i", str(path), "-map", "0:v?", "-map", "0:a?", "-f", "null", "-"])
    if case.get("tones"):
        check_tones(path, case["channels"], case.get("tone_frequencies"))
    if "quality_reference" in case:
        def decoded_luma(media):
            return run(["ffmpeg", "-v", "error", "-nostdin", "-i", str(media), "-map", "0:v:0",
                        "-pix_fmt", "gray", "-f", "rawvideo", "-"])
        reference = decoded_luma(case["quality_reference"])
        higher = decoded_luma(path)
        lower_path = Path(case["lower_quality_output"])
        lower = decoded_luma(lower_path)
        assert len(reference) == len(higher) == len(lower) > 0, f"{path}: quality changed frame count/geometry"
        higher_error = sum((a - b) ** 2 for a, b in zip(reference, higher)) / len(reference)
        lower_error = sum((a - b) ** 2 for a, b in zip(reference, lower)) / len(reference)
        assert higher_error < lower_error, f"{path}: the higher quality control did not improve decoded fidelity"
        assert path.stat().st_size > lower_path.stat().st_size, f"{path}: quality control produced no measurable size difference"
    if "lower_bitrate_output" in case:
        lower_path = Path(case["lower_bitrate_output"])
        lower_audio = [stream for stream in probe(lower_path)["streams"] if stream["codec_type"] == "audio"]
        assert int(audio[0]["bit_rate"]) > int(lower_audio[0]["bit_rate"]) * 1.2, f"{path}: explicit AAC bitrate did not change measured output rate"
        assert path.stat().st_size > lower_path.stat().st_size, f"{path}: bitrate control produced no measurable size difference"
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
        if "preview_source_seek" in case:
            actual = first_rgb(path, case["width"], case["height"])
            reference = first_rgb(FIXTURES / case["source"], case["width"], case["height"], case["preview_source_seek"])
            assert rgb_error(actual, reference) < 15, f"{path}: preview begins with the wrong source frame"
        if case.get("sdr_tone_map"):
            assert video[0].get("color_primaries") == "bt709", f"{path}: preview primaries remain HDR"
            assert video[0].get("color_transfer") == "bt709", f"{path}: preview transfer remains HDR"
            assert video[0].get("color_space") == "bt709", f"{path}: preview matrix remains HDR"
            assert video[0].get("pix_fmt") == "yuv420p", f"{path}: preview is not compatible 8-bit SDR"
            frames = json.loads(run(["ffprobe", "-v", "error", "-select_streams", "v:0", "-show_frames", "-of", "json", str(path)]))
            sides = [entry.get("side_data_type", "") for frame in frames["frames"] for entry in frame.get("side_data_list", [])]
            assert not any("mastering" in side.lower() or "content light" in side.lower() for side in sides), f"{path}: stale HDR mastering information"
            actual = first_rgb(path, case["width"], case["height"])
            reference = first_rgb(FIXTURES / case["source"], case["width"], case["height"], tone_map=True)
            unconverted = first_rgb(FIXTURES / case["source"], case["width"], case["height"])
            error = rgb_error(actual, reference)
            assert error < 15, f"{path}: SDR pixels differ from independent tone-map reference: {error}"
            assert error < rgb_error(actual, unconverted) * 0.75, f"{path}: HDR was only relabeled instead of tone mapped"
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
