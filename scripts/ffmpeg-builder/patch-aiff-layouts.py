#!/usr/bin/env python3
"""Repair exact AIFF speaker descriptions in the pinned FFmpeg 7.1.5 source.

Its legacy writer maps both back and side 5.1 to MPEG_5_1_A. Native CoreAudio
also distinguishes rear labels 33/34 from surround labels 5/6; copying a WAVE
mask into a CoreAudio bitmap does not preserve that distinction. Emit explicit
descriptions in the existing PCM sample order, and teach only the AIFF reader
their actual meaning. Existing MOV/CAF and AIFF bitmap/tag semantics stay intact.
"""
import hashlib
import os
from pathlib import Path
import sys
import tarfile
import tempfile


WRITER_OLD = """    if (par->ch_layout.order == AV_CHANNEL_ORDER_NATIVE && par->ch_layout.nb_channels > 2) {
        ffio_wfourcc(pb, "CHAN");
        avio_wb32(pb, 12);
        ff_mov_write_chan(pb, par->ch_layout.u.mask);
    }
"""

WRITER_NEW = """    if (par->ch_layout.order == AV_CHANNEL_ORDER_NATIVE && par->ch_layout.nb_channels > 2) {
        /* Preserve CoreAudio speaker identities and the native PCM order.
         * The legacy tag writer conflates back and side surround channels;
         * a CoreAudio bitmap also gives WAVE back-channel bits other names.
         * Velmux supports the known first 18 positions, never truncated masks. */
        if (par->ch_layout.u.mask >= 0x40000 ||
            par->ch_layout.nb_channels > 18 ||
            !av_channel_layout_check(&par->ch_layout))
            return AVERROR(ENOSYS);
        ffio_wfourcc(pb, "CHAN");
        avio_wb32(pb, 12 + 20 * par->ch_layout.nb_channels);
        avio_wb32(pb, 0); /* kAudioChannelLayoutTag_UseChannelDescriptions */
        avio_wb32(pb, 0);
        avio_wb32(pb, par->ch_layout.nb_channels);
        for (int ch = 0; ch < par->ch_layout.nb_channels; ch++) {
            enum AVChannel channel = av_channel_layout_channel_from_index(&par->ch_layout, ch);
            uint32_t label;
            switch (channel) {
            case AV_CHAN_BACK_LEFT:  label = 33; break; /* RearSurroundLeft */
            case AV_CHAN_BACK_RIGHT: label = 34; break; /* RearSurroundRight */
            case AV_CHAN_SIDE_LEFT:  label = 5;  break; /* LeftSurround */
            case AV_CHAN_SIDE_RIGHT: label = 6;  break; /* RightSurround */
            default: label = channel + 1; break;
            }
            avio_wb32(pb, label);
            avio_wb32(pb, 0); /* no coordinate flags */
            avio_wb32(pb, 0);
            avio_wb32(pb, 0);
            avio_wb32(pb, 0);
        }
    }
"""

# Put descriptions after the complete COMM chunk: independent older readers
# need its channel count before parsing UseChannelDescriptions.
WRITER_AFTER = "    if (  (par->codec_tag == MKTAG('Q','D','M','2')\n"

READER_OLD = """            tmp.u.map[i].id = mov_get_channel_id(label);
"""
READER_NEW = """            tmp.u.map[i].id = mov_get_channel_id(label);
            /* AIFF descriptions use actual CoreAudio speaker labels. Keep
             * other demuxers and existing AIFF tags/bitmaps unchanged. */
            if (s->iformat && !strcmp(s->iformat->name, "aiff")) {
                switch (label) {
                case 5:  tmp.u.map[i].id = AV_CHAN_SIDE_LEFT;  break;
                case 6:  tmp.u.map[i].id = AV_CHAN_SIDE_RIGHT; break;
                case 33: tmp.u.map[i].id = AV_CHAN_BACK_LEFT;  break;
                case 34: tmp.u.map[i].id = AV_CHAN_BACK_RIGHT; break;
                }
            }
"""

COUNT_OLD = """        int nb_channels = ch_layout->nb_channels;

        if (!num_descr || num_descr < nb_channels) {
"""
COUNT_NEW = """        int nb_channels = ch_layout->nb_channels;

        /* AIFF may put CHAN before COMM, so no channel count is known yet.
         * Bound the supported description domain and let COMM confirm it. */
        if (s->iformat && !strcmp(s->iformat->name, "aiff") && !nb_channels) {
            if (!num_descr || num_descr > 18)
                return AVERROR_INVALIDDATA;
            nb_channels = num_descr;
        }

        if (!num_descr || num_descr < nb_channels) {
"""

# Complete original/patched file hashes refuse source drift and partial patches.
PATCHES = [
    ("libavformat/aiffenc.c", "58a39d35931a6d6efff78cd7c28aa9ca794f16f1002ae25d5e4fb10b9db6bf80", "70f8ccf997d141698a9a4203c12b0193067732b5b74dcb24d02235de49e31454",
     [(WRITER_OLD, ""), (WRITER_AFTER, WRITER_NEW + "\n" + WRITER_AFTER)]),
    ("libavformat/mov_chan.c", "015cc3d3d2a2e5eff62d38b0fb3211c350054d30429cef10f5d04e2c6842a2f7", "37a4fedcc1957f69072a0ac2b1a9939e53668a7d95b72868927b939c2e7c8290",
     [("#include <stdint.h>\n", "#include <stdint.h>\n#include <string.h>\n"),
      (COUNT_OLD, COUNT_NEW), (READER_OLD, READER_NEW)]),
]


def apply(source):
    source = Path(source)
    if (source / "RELEASE").read_text().strip() != "7.1.5":
        raise ValueError("The AIFF layout repair requires pinned FFmpeg 7.1.5")
    pending = []
    for relative, original_hash, patched_hash, replacements in PATCHES:
        path = source / relative
        original = path.read_bytes()
        digest = hashlib.sha256(original).hexdigest()
        if digest == patched_hash:
            continue
        if digest != original_hash:
            raise ValueError(f"Refusing unexpected FFmpeg source: {relative}")
        text = original.decode()
        for old, new in replacements:
            if text.count(old) != 1:
                raise ValueError(f"The pinned AIFF repair did not match once: {relative}")
            text = text.replace(old, new, 1)
        patched = text.encode()
        if hashlib.sha256(patched).hexdigest() != patched_hash:
            raise ValueError(f"The pinned AIFF repair produced unexpected bytes: {relative}")
        pending.append((path, patched))
    for path, patched in pending:
        # Preserve resumability if a builder is interrupted while patching.
        with tempfile.NamedTemporaryFile(dir=path.parent, delete=False) as temporary:
            temporary_path = Path(temporary.name)
            try:
                temporary.write(patched)
                temporary.flush()
                os.fchmod(temporary.fileno(), path.stat().st_mode & 0o777)
                os.replace(temporary_path, path)
            finally:
                temporary_path.unlink(missing_ok=True)


def self_test(archive):
    archive = Path(archive)
    expected = "de668509caf9e35e3cd162473441fdb29538c6d96ed080292b3cf9e6fc5d558f"
    if hashlib.sha256(archive.read_bytes()).hexdigest() != expected:
        raise ValueError("The FFmpeg source archive does not match its pinned SHA256")
    with tempfile.TemporaryDirectory(prefix="velmux-aiff-source-") as directory:
        source = Path(directory)
        with tarfile.open(archive) as contents:
            for relative in ["RELEASE", *(entry[0] for entry in PATCHES)]:
                path = source / relative
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(contents.extractfile("ffmpeg-7.1.5/" + relative).read())
        apply(source)
        writer = (source / "libavformat/aiffenc.c").read_text()
        assert writer.index('ffio_wfourcc(pb, "COMM")') < writer.index('ffio_wfourcc(pb, "CHAN")') < writer.index('ffio_wfourcc(pb, "SSND")')
        once = [(source / entry[0]).read_bytes() for entry in PATCHES]
        apply(source)
        assert once == [(source / entry[0]).read_bytes() for entry in PATCHES]
        target = source / PATCHES[0][0]
        target.write_bytes(target.read_bytes() + b"\n/* unexpected source change */\n")
        try:
            apply(source)
        except ValueError:
            pass
        else:
            raise AssertionError("The source repair accepted unexpected source drift")
    print("Pinned AIFF writer/reader repair applies once, is idempotent and refuses source drift")


if __name__ == "__main__":
    if len(sys.argv) == 3 and sys.argv[1] == "--self-test":
        self_test(sys.argv[2])
    elif len(sys.argv) == 2:
        apply(sys.argv[1])
    else:
        raise SystemExit("usage: patch-aiff-layouts.py SOURCE | --self-test PINNED_TARBALL")
