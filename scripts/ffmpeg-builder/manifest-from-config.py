#!/usr/bin/env python3
"""Generate the FFmpeg capability manifest for cross-compiled targets.

Cross-compiled libraries cannot run velmux-ffmpeg-probe on the build host.
This script derives the manifest from the ACTUAL build tree:

- compiled codecs:  <build>/libavcodec/codec_list.c   (generated pointers)
- compiled formats: <build>/libavformat/{muxer,demuxer}_list.c
- media types:      <source>/libavcodec/codec_desc.c (codec descriptors)
- configure line:   <build>/ffbuild/config.mak

and emits the same JSON schema as the runtime probe:

{
  "ffmpeg": "<version>",
  "configuration": "<ffmpeg configure line>",
  "decoders": [{"name":..., "type":"video|audio|subtitle|..."}...],
  "encoders": [...],
  "muxers":   [{"name":..., "long_name": ""}...],
  "demuxers": [...],
  "hwaccels": []
}

Usage: manifest-from-config.py <ffmpeg-build-dir> <ffmpeg-source-dir>
                              <version> <manifest.json>
"""

import json
import re
import sys
from pathlib import Path

# Configure component names that differ from the runtime names the probe
# reports. Derived by diffing the generated component lists against the
# runtime probe output of an identical host build (see tools/ffmpeg-builder).
NAME_ALIASES = {
    "decoders": {
        "zero12v": "012v",
        "fourxm": "4xm",
        "eightbps": "8bps",
        "eightsvx_exp": "8svx_exp",
        "eightsvx_fib": "8svx_fib",
        "acelp_kelvin": "acelp.kelvin",
        "atrac3p": "atrac3plus",
        "atrac3pal": "atrac3plusal",
        "bink": "binkvideo",
        "cscd": "camstudio",
        "tscc": "camtasia",
        "ccaption": "cc_dec",
        "adpcm_g722": "g722",
        "adpcm_g726": "g726",
        "adpcm_g726le": "g726le",
        "hnm4_video": "hnm4video",
        "idcin": "idcinvideo",
        "iff_ilbm": "iff",
        "interplay_acm": "interplayacm",
        "interplay_video": "interplayvideo",
        "libvpx_vp8": "libvpx",
        "libvpx_vp9": "libvpx-vp9",
        "movtext": "mov_text",
        "msmpeg4v3": "msmpeg4",
        "ra_144": "real_144",
        "ra_288": "real_288",
        "roq": "roqvideo",
        "smacker": "smackvid",
        "ulti": "ultimotion",
        "vqa": "vqavideo",
        "ffwavesynth": "wavesynth",
    },
    "encoders": {
        "adpcm_g722": "g722",
        "adpcm_g726": "g726",
        "adpcm_g726le": "g726le",
        "libvpx_vp8": "libvpx",
        "libvpx_vp9": "libvpx-vp9",
        "movtext": "mov_text",
        "msmpeg4v3": "msmpeg4",
        "ra_144": "real_144",
        "roq": "roqvideo",
        "sonic_ls": "sonicls",
    },
    "muxers": {
        "matroska_audio": "matroska",
        "mpeg1system": "mpeg",
        "mpeg1vcd": "vcd",
        "mpeg2svcd": "svcd",
        "mpeg2dvd": "dvd",
        "mpeg2vob": "vob",
        "pcm_alaw": "alaw",
        "pcm_mulaw": "mulaw",
        "pcm_f32be": "f32be",
        "pcm_f32le": "f32le",
        "pcm_f64be": "f64be",
        "pcm_f64le": "f64le",
        "pcm_s16be": "s16be",
        "pcm_s16le": "s16le",
        "pcm_s24be": "s24be",
        "pcm_s24le": "s24le",
        "pcm_s32be": "s32be",
        "pcm_s32le": "s32le",
        "pcm_s8": "s8",
        "pcm_u16be": "u16be",
        "pcm_u16le": "u16le",
        "pcm_u24be": "u24be",
        "pcm_u24le": "u24le",
        "pcm_u32be": "u32be",
        "pcm_u32le": "u32le",
        "pcm_u8": "u8",
        "pcm_vidc": "vidc",
        "segafilm": "film_cpk",
        "stream_segment": "stream_segment,ssegment",
        "tg2": "3g2",
        "tgp": "3gp",
        "vc1t": "vc1test",
    },
    "demuxers": {
        "bintext": "bin",
        "fourxm": "4xm",
        "image2_alias_pix": "alias_pix",
        "image2_brender_pix": "brender_pix",
        "image_bmp_pipe": "bmp_pipe",
        "image_cri_pipe": "cri_pipe",
        "image_dds_pipe": "dds_pipe",
        "image_dpx_pipe": "dpx_pipe",
        "image_exr_pipe": "exr_pipe",
        "image_gem_pipe": "gem_pipe",
        "image_gif_pipe": "gif_pipe",
        "image_hdr_pipe": "hdr_pipe",
        "image_j2k_pipe": "j2k_pipe",
        "image_jpeg_pipe": "jpeg_pipe",
        "image_jpegls_pipe": "jpegls_pipe",
        "image_jpegxl_pipe": "jpegxl_pipe",
        "image_pam_pipe": "pam_pipe",
        "image_pbm_pipe": "pbm_pipe",
        "image_pcx_pipe": "pcx_pipe",
        "image_pfm_pipe": "pfm_pipe",
        "image_pgm_pipe": "pgm_pipe",
        "image_pgmyuv_pipe": "pgmyuv_pipe",
        "image_pgx_pipe": "pgx_pipe",
        "image_phm_pipe": "phm_pipe",
        "image_photocd_pipe": "photocd_pipe",
        "image_pictor_pipe": "pictor_pipe",
        "image_png_pipe": "png_pipe",
        "image_ppm_pipe": "ppm_pipe",
        "image_psd_pipe": "psd_pipe",
        "image_qdraw_pipe": "qdraw_pipe",
        "image_qoi_pipe": "qoi_pipe",
        "image_sgi_pipe": "sgi_pipe",
        "image_sunrast_pipe": "sunrast_pipe",
        "image_svg_pipe": "svg_pipe",
        "image_tiff_pipe": "tiff_pipe",
        "image_vbn_pipe": "vbn_pipe",
        "image_webp_pipe": "webp_pipe",
        "image_xbm_pipe": "xbm_pipe",
        "image_xpm_pipe": "xpm_pipe",
        "image_xwd_pipe": "xwd_pipe",
        "matroska": "matroska,webm",
        "mov": "mov,mp4,m4a,3gp,3g2,mj2",
        "mpegps": "mpeg",
        "msnwc_tcp": "msnwctcp",
        "pcm_alaw": "alaw",
        "pcm_mulaw": "mulaw",
        "pcm_f32be": "f32be",
        "pcm_f32le": "f32le",
        "pcm_f64be": "f64be",
        "pcm_f64le": "f64le",
        "pcm_s16be": "s16be",
        "pcm_s16le": "s16le",
        "pcm_s24be": "s24be",
        "pcm_s24le": "s24le",
        "pcm_s32be": "s32be",
        "pcm_s32le": "s32le",
        "pcm_s8": "s8",
        "pcm_u16be": "u16be",
        "pcm_u16le": "u16le",
        "pcm_u24be": "u24be",
        "pcm_u24le": "u24le",
        "pcm_u32be": "u32be",
        "pcm_u32le": "u32le",
        "pcm_u8": "u8",
        "pcm_vidc": "vidc",
        "segafilm": "film_cpk",
        "shorten": "shn",
        "smacker": "smk",
        "str": "psxstr",
        "threedostr": "3dostr",
        "vc1t": "vc1test",
        "wc3": "wc3movie",
    },
}

# Variant/alternate codecs that have no AVCodecDescriptor entry under their
# decoder name (e.g. the DTS decoder is "dca" but the descriptor is "dts";
# *_fixed/*float are alternate implementations of the same codec). Types
# below were cross-checked against the runtime probe of an identical build.
DESCRIPTOR_TYPE_OVERRIDES = {
    "012v": "video",
    "4xm": "video",
    "8bps": "video",
    "8svx_exp": "audio",
    "8svx_fib": "audio",
    "a64multi": "video",
    "a64multi5": "video",
    "aac_fixed": "audio",
    "ac3_fixed": "audio",
    "acelp.kelvin": "audio",
    "als": "audio",
    "amrnb": "audio",
    "amrwb": "audio",
    "binkvideo": "video",
    "cc_dec": "subtitle",
    "dca": "audio",
    "dvbsub": "subtitle",
    "dvdsub": "subtitle",
    "eacmv": "video",
    "eamad": "video",
    "eatgq": "video",
    "eatgv": "video",
    "eatqi": "video",
    "flv": "video",
    "hnm4video": "video",
    "interplayacm": "audio",
    "interplayvideo": "video",
    "mov_text": "subtitle",
    "mp1float": "audio",
    "mp2fixed": "audio",
    "mp2float": "audio",
    "mp3adufloat": "audio",
    "mp3float": "audio",
    "mp3on4float": "audio",
    "mpc7": "audio",
    "mpc8": "audio",
    "mpegvideo": "video",
    "on2avc": "audio",
    "pgssub": "subtitle",
    "prores_aw": "video",
    "prores_ks": "video",
    "smackaud": "audio",
    "smackvid": "video",
    "sonicls": "audio",
    "vc2": "video",
    "vqavideo": "video",
    "wavesynth": "audio",
    "ws_snd1": "audio",
    "xl": "video",
}

# External codec libraries have no AVCodecDescriptor; their media types are
# stable public facts of the codecs they wrap.
EXTERNAL_TYPES = {
    "libx264": "video",
    "libx264rgb": "video",
    "libx265": "video",
    "libsvtav1": "video",
    "libvpx": "video",
    "libvpx_vp8": "video",
    "libvpx_vp9": "video",
    "libmp3lame": "audio",
    "libopus": "audio",
    "libvorbis": "audio",
    "libaom": "video",
    "libaom_av1": "video",
    "libdav1d": "video",
    "libcodec2": "audio",
    "libilbc": "audio",
    "libopencore_amrnb": "audio",
    "libopencore_amrwb": "audio",
    "libopenh264": "video",
    "librav1e": "video",
    "libspeex": "audio",
    "libtheora": "video",
    "libtwolame": "audio",
    "libvo_amrwbenc": "audio",
    "libvorbis": "audio",
    "libzvbi": "subtitle",
}

MEDIA_TYPES = {
    "VIDEO": "video",
    "AUDIO": "audio",
    "SUBTITLE": "subtitle",
    "DATA": "data",
    "ATTACHMENT": "attachment",
}


def parse_pointer_list(path: Path, symbol_prefix: str) -> list:
    """Extract component names from a generated '&ff_<name>_<kind>,' list."""
    if not path.exists():
        return []
    entries = []
    pattern = re.compile(r"&ff_([a-z0-9_]+)_(" + symbol_prefix + r"),")
    for match in pattern.finditer(path.read_text()):
        entries.append(match.group(1))
    return entries


def parse_codec_descriptors(source_dir: Path) -> dict:
    """name -> media type from libavcodec/codec_desc.c."""
    desc_file = source_dir / "libavcodec" / "codec_desc.c"
    types = {}
    if not desc_file.exists():
        return types
    text = desc_file.read_text()
    block_re = re.compile(
        r"\.id\s*=\s*AV_CODEC_ID_\w+,\s*\.type\s*=\s*AVMEDIA_TYPE_(\w+),"
        r"\s*\.name\s*=\s*\"([^\"]+)\"", re.S)
    for match in block_re.finditer(text):
        types[match.group(2)] = MEDIA_TYPES.get(match.group(1), "other")
    return types


def read_configuration(build_dir: Path) -> str:
    mak = build_dir / "ffbuild" / "config.mak"
    if mak.exists():
        for line in mak.read_text().splitlines():
            if line.startswith("FFMPEG_CONFIGURATION="):
                return line.split("=", 1)[1].strip()
    return ""


def main() -> int:
    if len(sys.argv) != 5:
        print("usage: manifest-from-config.py <ffmpeg-build-dir> "
              "<ffmpeg-source-dir> <version> <manifest.json>", file=sys.stderr)
        return 2
    build_dir = Path(sys.argv[1])
    source_dir = Path(sys.argv[2])
    version = sys.argv[3]
    manifest_path = Path(sys.argv[4])

    codec_list = build_dir / "libavcodec" / "codec_list.c"
    if not codec_list.exists():
        print(f"error: {codec_list} not found (configure FFmpeg first)",
              file=sys.stderr)
        return 1

    descriptor_types = parse_codec_descriptors(source_dir)

    def codec_entries(kind):
        entries = []
        alias_map = NAME_ALIASES[kind + "s"]
        for component in parse_pointer_list(codec_list, kind):
            name = alias_map.get(component, component)
            media = (DESCRIPTOR_TYPE_OVERRIDES.get(name)
                     or descriptor_types.get(component)
                     or descriptor_types.get(component.rsplit("_", 1)[0])
                     or EXTERNAL_TYPES.get(component, "other"))
            entries.append({"name": name, "type": media})
        return entries

    def format_entries(kind):
        list_name = "muxer_list.c" if kind == "muxers" else "demuxer_list.c"
        alias_map = NAME_ALIASES[kind]
        entries = []
        for component in parse_pointer_list(
                build_dir / "libavformat" / list_name, kind[:-1]):
            name = alias_map.get(component, component)
            entries.append({"name": name, "long_name": ""})
        return entries

    manifest = {
        "ffmpeg": version,
        "configuration": read_configuration(build_dir),
        "decoders": codec_entries("decoder"),
        "encoders": codec_entries("encoder"),
        "muxers": format_entries("muxers"),
        "demuxers": format_entries("demuxers"),
        "filters": [{"name": name} for name in re.findall(
            r"&ff_(?:af|vf|avf|asrc|vsrc|avsrc|asink|vsink|avsink)_([a-z0-9_]+),",
            (build_dir / "libavfilter/filter_list.c").read_text())],
        "hwaccels": [{"name": name} for name in parse_pointer_list(
            build_dir / "libavcodec/hwaccel_list.c", "hwaccel")],
        "evidence": "generated component lists; runtime verification is required on target",

    }

    manifest_path.parent.mkdir(parents=True, exist_ok=True)
    manifest_path.write_text(json.dumps(manifest, indent=1) + "\n")
    print(f"manifest OK: {len(manifest['decoders'])} decoders, "
          f"{len(manifest['encoders'])} encoders, "
          f"{len(manifest['muxers'])} muxers, "
          f"{len(manifest['demuxers'])} demuxers")
    return 0


if __name__ == "__main__":
    sys.exit(main())
