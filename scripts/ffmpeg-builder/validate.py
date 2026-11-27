#!/usr/bin/env python3
"""Validate the actual FFmpeg build against the Velmux feature requirements.

Reads the capability manifest produced by velmux-ffmpeg-probe (built against
the real libraries) and asserts that every codec/container/filter required by
the Velmux V1 feature matrix is present. Exits non-zero with a clear message
on the first missing capability.

Usage: validate.py <manifest.json>
"""

import json
import sys

REQUIRED_DECODERS = {
    # video: modern/common
    "h264", "hevc", "av1", "vp8", "vp9",
    # video: compatibility/legacy
    "mpeg4", "mpeg2video", "mpeg1video", "h263", "vc1", "wmv3", "wmv1", "wmv2",
    # video: professional/archival
    "prores", "dnxhd", "ffv1", "mjpeg",
    # audio
    "aac", "mp3", "opus", "vorbis", "flac", "alac", "pcm_s16le", "ac3", "eac3",
    "dca", "truehd", "wmapro", "wmav2", "wavpack", "ape", "amrnb", "amrwb",
    # subtitles (FFmpeg 7.1 names; TTML decode is demux-level in this pin
    # and is tracked as a known limitation in docs/FEATURE_MATRIX.md)
    "srt", "subrip", "webvtt", "ass", "ssa", "mov_text", "text",
    "dvbsub", "pgssub", "dvdsub",
}

REQUIRED_ENCODERS = {
    # video
    "libx264", "libx265", "libsvtav1", "libvpx", "libvpx-vp9", "mpeg4", "ffv1",
    "mjpeg", "libx264rgb",
    # audio
    "aac", "libmp3lame", "libopus", "libvorbis", "flac", "pcm_s16le", "ac3",
    "wavpack",
    # subtitles
    "srt", "subrip", "webvtt", "ass", "mov_text", "text",
}

REQUIRED_MUXERS = {
    "mp4", "mov", "matroska", "webm", "avi", "mpegts", "flv", "3gp", "mp3",
    "wav", "ogg", "adts",
}

REQUIRED_DEMUXERS = {
    "mov,mp4,m4a,3gp,3g2,mj2", "matroska,webm", "avi", "mpegts", "flv",
    "asf", "wav", "mp3", "ogg", "flac", "wv", "ape", "amr",
}

# Require actual compiled filter entries rather than inferring them from flags.
REQUIRED_FILTERS = {
    "subtitles", "ass", "crop", "trim", "atrim", "concat", "scale", "zscale",
    "tonemap", "reverse", "areverse", "fps", "transpose", "hflip", "vflip",
    "atempo", "loudnorm", "volume", "afade", "silenceremove", "amix", "pad",
    "overlay", "select", "aselect", "setpts", "asetpts",
}


def main() -> int:
    args = sys.argv[1:]
    exempt = set()
    if args and args[0] == "--exempt":
        if len(args) < 2:
            print("usage: validate.py [--exempt n1,n2,...] <manifest.json>",
                  file=sys.stderr)
            return 2
        exempt = {e.strip() for e in args[1].split(",") if e.strip()}
        args = args[2:]
    if len(args) != 1:
        print("usage: validate.py [--exempt n1,n2,...] <manifest.json>",
                  file=sys.stderr)
        return 2
    with open(args[0], encoding="utf-8") as fh:
        manifest = json.load(fh)

    problems = []

    decoders = {c["name"] for c in manifest.get("decoders", [])}
    encoders = {c["name"] for c in manifest.get("encoders", [])}
    muxers = {f["name"] for f in manifest.get("muxers", [])}
    demuxers = {f["name"] for f in manifest.get("demuxers", [])}

    missing = sorted(REQUIRED_DECODERS - decoders - exempt)
    if missing:
        problems.append("missing decoders: " + ", ".join(missing))
    missing = sorted(REQUIRED_ENCODERS - encoders - exempt)
    if missing:
        problems.append("missing encoders: " + ", ".join(missing))
    missing = sorted(REQUIRED_MUXERS - muxers - exempt)
    if missing:
        problems.append("missing muxers: " + ", ".join(missing))
    missing = sorted(REQUIRED_DEMUXERS - demuxers - exempt)
    if missing:
        problems.append("missing demuxers: " + ", ".join(missing))

    filters = {f["name"] for f in manifest.get("filters", [])}
    missing = sorted(REQUIRED_FILTERS - filters)
    if missing:
        problems.append("missing filters: " + ", ".join(missing))

    config = manifest.get("configuration", "")
    for flag in (
        "--enable-libx264",
        "--enable-libmp3lame",
        "--enable-libopus",
        "--enable-libvorbis",
        "--enable-libvpx",
        "--enable-libdav1d",
        "--enable-libsvtav1",
        "--enable-libass",
    ):
        # An exemption (e.g. "libsvtav1" on 32-bit ABIs where upstream does
        # not support it) also covers its configure flag.
        short = flag.removeprefix("--enable-")
        if flag not in config and short not in exempt:
            problems.append("configure flag absent: " + flag)
    target_encoders = set()
    target_decoders = set()
    if "--enable-mediacodec" in config:
        target_encoders.update({"h264_mediacodec", "hevc_mediacodec"})
        target_decoders.update({"h264_mediacodec", "hevc_mediacodec"})
    if "--enable-videotoolbox" in config:
        target_encoders.update({"h264_videotoolbox", "hevc_videotoolbox"})
    missing = sorted(target_encoders - encoders)
    if missing:
        problems.append("missing platform encoders: " + ", ".join(missing))
    missing = sorted(target_decoders - decoders)
    if missing:
        problems.append("missing platform decoders: " + ", ".join(missing))
    if "--disable-avdevice" not in config:
        problems.append("libavdevice should be disabled but is not")

    if problems:
        print("FFmpeg build validation FAILED:", file=sys.stderr)
        for p in problems:
            print("  - " + p, file=sys.stderr)
        return 1

    print(
        "FFmpeg build validation OK: "
        f"{len(decoders)} decoders, {len(encoders)} encoders, "
        f"{len(muxers)} muxers, {len(demuxers)} demuxers "
        f"(ffmpeg {manifest.get('ffmpeg', '?')})"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
