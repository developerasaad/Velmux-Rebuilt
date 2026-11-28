#!/usr/bin/env python3
"""Inspect and decode real core outputs with independently installed FFmpeg tools."""
import array
import hashlib
import json
import math
import re
import struct
from pathlib import Path
import subprocess
import sys
from fractions import Fraction

ROOT = Path(__file__).resolve().parents[2]
FIXTURES = ROOT / "build/media-fixtures"
OUTPUTS = ROOT / "build/media-test-output"
AIFF_LAYOUT_EVIDENCE = []


def run(arguments):
    return subprocess.check_output(arguments, stderr=subprocess.PIPE)


def probe(path, packets=False):
    command = ["ffprobe", "-v", "error", "-show_streams", "-show_format", "-show_chapters"]
    if packets:
        command += ["-show_packets", "-show_data_hash", "sha256"]
    return json.loads(run(command + ["-of", "json", str(path)]))


def media_packets(info, kind):
    indices = {stream["index"] for stream in info["streams"] if stream["codec_type"] == kind}
    return [packet for packet in info.get("packets", []) if packet["stream_index"] in indices]


def assert_close(actual, expected, tolerance, detail):
    assert math.isfinite(actual) and abs(actual - expected) <= tolerance, f"{detail}: {actual} != {expected} ± {tolerance}"


def check_tones(path, expected_channels, expected_frequencies=None, samples=None):
    if samples is None:
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


def decoded_audio(path, representation='s32le', codec='pcm_s32le'):
    return run(['ffmpeg', '-v', 'error', '-nostdin', '-i', str(path), '-map', '0:a:0',
                '-f', representation, '-acodec', codec, '-'])


def float_samples(raw):
    values = array.array('d')
    values.frombytes(raw)
    if sys.byteorder != 'little':
        values.byteswap()
    return values


def check_audio_speed(path, case):
    rate, channels = case['sample_rate'], case['channels']
    actual = float_samples(decoded_audio(path, 'f64le', 'pcm_f64le'))
    source = float_samples(decoded_audio(FIXTURES / case['speed_source'], 'f64le', 'pcm_f64le'))
    frames = len(actual) // channels
    ideal = len(source) / channels * 1000 / case['speed_milli']
    # WSOLA's boundary is content-dependent. Pinned atempo uses a power-of-two
    # rate/24 window; asetrate/resampling has only sample rounding here.
    window = 1 << max(0, (max(1, rate // 24) - 1).bit_length())
    tolerance = 2 * window if case['preserve_pitch'] else 2
    assert abs(frames - ideal) <= tolerance, f'{path}: transformed sample count {frames}, expected {ideal} ± {tolerance}'
    begin = min(frames // 4, rate // 4)
    count = min(frames // 4, rate // 4)
    assert count > rate // 20, f'{path}: missing audible speed samples'
    for channel in range(channels):
        values = actual[begin * channels + channel:(begin + count) * channels:channels]
        frequency = ([440, 660][channel] if channels == 2 else 440) * (1 if case['preserve_pitch'] else case['speed_milli'] / 1000)
        amplitude = 2 * math.hypot(
            sum(value * math.cos(2 * math.pi * frequency * i / rate) for i, value in enumerate(values)),
            sum(value * math.sin(2 * math.pi * frequency * i / rate) for i, value in enumerate(values))) / len(values)
        gain = 10 ** (case.get('speed_gain_centidb', 0) / 2000)
        assert 0.08 * gain < amplitude < 0.2 * gain, f'{path}: accepted pitch/channel tone {frequency} Hz is missing: {amplitude}'
    if 'speed_unfaded_output' in case:
        reference = float_samples(decoded_audio(case['speed_unfaded_output'], 'f64le', 'pcm_f64le'))
        assert len(actual) == len(reference), f'{path}: EOF fade changed transformed sample count'
        fade_in = case['speed_fade_in_us'] * rate / 1_000_000
        fade_out = case['speed_fade_out_us'] * rate / 1_000_000
        gain = 10 ** (case.get('speed_gain_centidb', 0) / 2000)
        def faded(index, value):
            sample = index // channels
            return value * gain * max(0, min(1, sample / fade_in, (frames - sample) / fade_out))
        error = max(abs(value - faded(index, reference[index])) for index, value in enumerate(actual))
        assert error <= 3 / 32768, f'{path}: speed fade does not reach the actual sample endpoint: {error}'
        peak = max(abs(value) for value in actual[-channels:])
        assert peak <= 2 / 32768, f'{path}: audible truncated EOF fade: last sample peak {peak}'
    elif not case['preserve_pitch']:
        # A separately installed tool performs the exact accepted representation,
        # including the integer-rate bridge needed at 11025 Hz × 0.5.
        speed = case['speed_milli']
        processing = rate if rate * speed % 1000 == 0 else math.ceil(rate / 48000) * 48000
        chain = (f'aresample={processing},' if processing != rate else '') + f'asetrate={processing * speed // 1000},aresample={rate}'
        raw = run(['ffmpeg', '-v', 'error', '-nostdin', '-i', str(FIXTURES / case['speed_source']),
                   '-af', chain, '-f', 'f64le', '-acodec', 'pcm_f64le', '-'])
        reference = float_samples(raw)
        assert len(reference) == len(actual), f'{path}: pitch-following sample sequence length differs from independent resampling'
        error = max(abs(a - b) for a, b in zip(actual, reference))
        assert error <= 3 / 32768, f'{path}: pitch-following samples/precision/channel order differ: {error}'


def check_audio_speed_timing(path, case):
    source = FIXTURES / case['timing_speed_source']
    document = probe(source)
    source_origin = min(float(stream['start_time']) for stream in document['streams'])
    result = probe(path)
    speed = case['speed_milli'] / 1000
    for index, stream in enumerate(document['streams']):
        rate, channels = int(stream['sample_rate']), stream['channels']
        frames_doc = json.loads(run(['ffprobe', '-v', 'error', '-select_streams', f'a:{index}',
                                    '-show_frames', '-of', 'json', str(source)]))['frames']
        raw = run(['ffmpeg', '-v', 'error', '-nostdin', '-i', str(source), '-map', f'0:a:{index}',
                   '-f', 'f64le', '-acodec', 'pcm_f64le', '-'])
        decoded = float_samples(raw)
        reconstructed = []
        consumed = 0
        start = float(stream['start_time'])
        time_base = Fraction(stream['time_base'])
        quantization = max(1, math.ceil(float(time_base) * rate))
        for frame in frames_doc:
            position = round((float(frame['best_effort_timestamp_time']) - start) * rate)
            count = int(frame['nb_samples'])
            current = len(reconstructed) // channels
            assert position >= current - quantization, f'{source}: fixture unexpectedly overlaps audio frames'
            if position > current + quantization:
                reconstructed.extend([0.0] * ((position - current) * channels))
            reconstructed.extend(decoded[consumed:consumed + count * channels])
            consumed += count * channels
        assert consumed == len(decoded), f'{source}: independent fixture frame/sample accounting differs'
        # Independent resampling of the timestamp-reconstructed PCM verifies the
        # actual long silence, not merely a container duration or first tone.
        command = ['ffmpeg', '-v', 'error', '-nostdin', '-f', 'f64le', '-ar', str(rate), '-ac', str(channels), '-i', '-',
                   '-af', f'asetrate={rate * case["speed_milli"] // 1000},aresample={rate}', '-f', 'f64le', '-acodec', 'pcm_f64le', '-']
        payload = array.array('d', reconstructed)
        if sys.byteorder != 'little': payload.byteswap()
        expected = float_samples(subprocess.run(command, input=payload.tobytes(), stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=True).stdout)
        actual = float_samples(run(['ffmpeg', '-v', 'error', '-nostdin', '-i', str(path), '-map', f'0:a:{index}', '-f', 'f64le', '-acodec', 'pcm_f64le', '-']))
        assert len(actual) == len(expected), f'{path}: speed dropped timestamp silence or track samples: {len(actual)} != {len(expected)}'
        assert max(abs(a - b) for a, b in zip(actual, expected)) <= 3 / 32768, f'{path}: independently reconstructed audio timeline differs'
        expected_start = (start - source_origin) / speed
        output_start = result['streams'][index].get('start_time')
        if output_start is None:
            output_frames = json.loads(run(['ffprobe', '-v', 'error', '-select_streams', f'a:{index}',
                                            '-show_frames', '-of', 'json', str(path)]))['frames']
            assert output_frames and 'best_effort_timestamp_time' in output_frames[0], f'{path}: missing independently decoded track origin'
            output_start = output_frames[0]['best_effort_timestamp_time']
        assert_close(float(output_start), expected_start, 0.0011, f'{path}: transformed track origin')
    if case['timing_speed_source'] == 'audio-gap.mkv':
        assert len(reconstructed) // channels >= rate * 3.9, 'Long-gap fixture no longer has a gap exceeding one second'


def described_aiff_pcm(path, case):
    """Check actual CoreAudio labels and decode PCM without guessing speakers.

    Unpatched independent FFmpeg may not recognize CoreAudio rear labels 33/34.
    Parse their standard CHAN descriptions and SSND samples independently; never
    accept that decoder's guessed layout as evidence of physical speaker identity.
    """
    raw = path.read_bytes()
    assert raw[:4] == b'FORM' and raw[8:12] in (b'AIFF', b'AIFC'), path
    assert struct.unpack_from('>I', raw, 4)[0] == len(raw) - 8, f'{path}: unfinished AIFF FORM'
    chunks = {}
    position = 12
    while position + 8 <= len(raw):
        tag = raw[position:position + 4]
        size = struct.unpack_from('>I', raw, position + 4)[0]
        end = position + 8 + size
        assert end <= len(raw), f'{path}: truncated AIFF chunk'
        assert tag not in chunks, f'{path}: duplicate AIFF chunk'
        chunks[tag] = raw[position + 8:end]
        position = end + (size & 1)
    assert position == len(raw), f'{path}: unparsed AIFF bytes'
    common, chan, sound = (chunks[key] for key in (b'COMM', b'CHAN', b'SSND'))
    channels, frames, bits = struct.unpack_from('>HIH', common)
    exponent, mantissa = struct.unpack_from('>HQ', common, 8)
    assert exponent < 0x7fff and mantissa > 0, f'{path}: invalid AIFF sample rate'
    rate = math.ldexp(mantissa, exponent - 16383 - 63)
    assert channels == case['channels'] and rate == case['sample_rate'], path
    layout, bitmap, count = struct.unpack_from('>III', chan)
    assert layout == bitmap == 0 and count == channels and len(chan) == 12 + count * 20, f'{path}: missing exact speaker descriptions'
    labels = []
    for index in range(count):
        label, flags, x, y, z = struct.unpack_from('>IIIII', chan, 12 + 20 * index)
        assert flags == x == y == z == 0, f'{path}: unexpected coordinate speaker mapping'
        labels.append(label)
    expected = {'5.1': [1, 2, 3, 4, 33, 34], '5.1(side)': [1, 2, 3, 4, 5, 6],
                '7.1': [1, 2, 3, 4, 33, 34, 5, 6]}[case['channel_layout']]
    assert labels == expected, f'{path}: actual CoreAudio speaker identities/order changed: {labels}'
    offset, block = struct.unpack_from('>II', sound)
    assert block == 0, f'{path}: unsupported blocked AIFF PCM'
    pcm = sound[8 + offset:]
    width = (bits + 7) // 8
    assert len(pcm) == frames * channels * width, f'{path}: AIFF sample count/precision changed'
    compression = common[18:22] if raw[8:12] == b'AIFC' else b'NONE'
    if compression.lower() in (b'fl32', b'fl64'):
        assert bits == (32 if compression.lower() == b'fl32' else 64), path
        values = [item[0] for item in struct.iter_unpack('>f' if bits == 32 else '>d', pcm)]
        integer = None
    else:
        assert compression in (b'NONE', b'twos', b'sowt') and bits in (8, 16, 24, 32), path
        order = 'little' if compression == b'sowt' else 'big'
        integers = [int.from_bytes(pcm[index:index + width], order, signed=True) for index in range(0, len(pcm), width)]
        values = [value / 2 ** (bits - 1) for value in integers]
        integer = b''.join((value << (32 - bits)).to_bytes(4, 'little', signed=True) for value in integers)
    assert values and all(math.isfinite(value) and abs(value) <= 1 for value in values), path
    return {'values': values, 's32': integer, 'frames': frames, 'labels': labels, 'bits': bits}


def verify_aiff_decoding(path, frames):
    # Decode the whole AIFF with independent libavcodec through ffprobe. This
    # needs no resampling/filter graph that could guess or relabel unknown labels.
    decoded = subprocess.run(['ffprobe', '-v', 'error', '-err_detect', 'explode', '-select_streams', 'a:0',
                              '-show_frames', '-of', 'json', str(path)], stdout=subprocess.PIPE,
                             stderr=subprocess.PIPE, check=True)
    assert not decoded.stderr.strip(), f'{path}: independent AIFF decoding reported errors: {decoded.stderr.decode()}'
    decoded_frames = json.loads(decoded.stdout)['frames']
    assert sum(int(frame['nb_samples']) for frame in decoded_frames) == frames, f'{path}: independently decoded AIFF sample count changed'


def first_rgb(path, width, height, start=0, tone_map=False):
    filters = []
    if tone_map:
        filters += ["zscale=t=linear:npl=100", "format=gbrpf32le", "zscale=p=bt709",
                    "tonemap=tonemap=hable:desat=0", "zscale=t=bt709:m=bt709:r=limited", "format=yuv420p"]
    filters.append(f"scale={width}:{height}:flags=bicubic")
    # Decode the first presentation frame sequentially. An unnecessary demuxer
    # seek to zero can skip an FLV's initial keyframe when reordered video and
    # audio have different positive starting timestamps.
    seek = ["-ss", str(start)] if start > 0 else []
    return run(["ffmpeg", "-v", "error", "-nostdin", "-threads", "1", *seek,
                "-i", str(path), "-map", "0:v:0", "-vf", ",".join(filters),
                "-frames:v", "1", "-pix_fmt", "rgb24", "-f", "rawvideo", "-"])


def rgb_error(actual, expected):
    assert len(actual) == len(expected) and actual, "A selected preview frame was not decoded"
    return math.sqrt(sum((a - b) ** 2 for a, b in zip(actual, expected)) / len(actual))


def check_case(case):
    path = Path(case["path"])
    assert path.is_file() and path.stat().st_size > 64, f"Missing/nontrivial output: {path}"
    info = probe(path, packets=True)
    if "container" in case:
        assert case["container"] in info["format"]["format_name"].split(','), f"{path}: output container differs from the accepted format"
    pictures = [stream for stream in info["streams"] if stream["codec_type"] == "video" and stream.get("disposition", {}).get("attached_pic")]
    video = [stream for stream in info["streams"] if stream["codec_type"] == "video" and not stream.get("disposition", {}).get("attached_pic")]
    audio = [stream for stream in info["streams"] if stream["codec_type"] == "audio"]
    described_aiff = None
    if 'channel_layout' in case and 'aiff' in info['format']['format_name'].split(','):
        described_aiff = described_aiff_pcm(path, case)
        verify_aiff_decoding(path, described_aiff['frames'])
        AIFF_LAYOUT_EVIDENCE.append({'path': str(path.relative_to(OUTPUTS)),
                                    'coreaudio_labels': described_aiff['labels'],
                                    'verified_layout': case['channel_layout'],
                                    'independent_ffprobe_layout': audio[0].get('channel_layout'),
                                    'decoded_sample_frames': described_aiff['frames'],
                                    'verification': 'Exact CHAN speaker identities, independent PCM values and full ffprobe decoding'})
    if "attached_picture_codec" in case:
        assert len(pictures) == 1 and pictures[0]["codec_name"] == case["attached_picture_codec"], f"{path}: attached artwork changed"
        assert (pictures[0]["width"], pictures[0]["height"]) == (32, 32), f"{path}: artwork geometry changed"
    else:
        assert not pictures, f"{path}: unexpected attached artwork"
    if "video_codec" in case:
        assert len(video) == 1 and video[0]["codec_name"] == case["video_codec"], path
        assert (video[0]["width"], video[0]["height"]) == (case["width"], case["height"]), path
        for property_name, expected_name in [('pix_fmt', 'pixel_format'), ('color_transfer', 'color_transfer'),
                                               ('color_primaries', 'color_primaries'), ('color_space', 'color_space')]:
            if expected_name in case:
                assert video[0].get(property_name) == case[expected_name], f'{path}: {property_name} differs from accepted representation'
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
        if 'audio_bit_rate' in case:
            assert int(stream.get('bit_rate', 0)) == case['audio_bit_rate'], f'{path}: encoded audio bitrate differs from the visible accepted control'
        if 'channel_layout' in case:
            if described_aiff is None:
                assert stream.get('channel_layout') == case['channel_layout'], f'{path}: speaker layout changed'
        if 'audio_bits' in case:
            bits = int(stream.get('bits_per_raw_sample', 0) or stream.get('bits_per_sample', 0))
            assert bits == case['audio_bits'], f'{path}: sample precision changed: {bits}'
    if 'estimated_pcm_payload_bytes' in case:
        # Measure the produced sample payload independently of Rust's estimate
        # and distinguish it from container/metadata bytes in the saved file.
        assert len(audio) == 1 and audio[0]['codec_name'] == 'pcm_s24le', path
        payload = decoded_audio(path, 's24le', 'pcm_s24le')
        assert len(payload) == case['estimated_pcm_payload_bytes'], f'{path}: actual PCM payload differs from the sample-count projection'
        assert path.stat().st_size > len(payload), f'{path}: media payload was misrepresented as finalized file bytes'
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
        if described_aiff is not None:
            assert described_aiff['frames'] == case['decoded_samples'], f'{path}: AIFF decoded sample count changed'
        else:
            raw = run(["ffmpeg", "-v", "error", "-nostdin", "-i", str(path), "-map", "0:a:0", "-f", "f32le", "-acodec", "pcm_f32le", "-"])
            assert len(raw) == case["decoded_samples"] * case["channels"] * 4, f"{path}: decoded sample count changed"
    if "duration_range" in case:
        lower, upper = case["duration_range"]
        assert lower <= float(info["format"]["duration"]) <= upper, f"{path}: preview is not bounded to its selected interval"
    if "reported_preview_duration_us" in case:
        assert_close(float(info["format"]["duration"]), case["reported_preview_duration_us"] / 1_000_000, 0.15, f"{path}: reported preview duration")
    if described_aiff is None:
        run(["ffmpeg", "-v", "error", "-xerror", "-err_detect", "explode", "-nostdin", "-threads", "1",
             "-i", str(path), "-map", "0:v?", "-map", "0:a?", "-f", "null", "-"])
    if case.get("tones"):
        check_tones(path, case["channels"], case.get("tone_frequencies"),
                    described_aiff['values'] if described_aiff is not None else None)
    if 'exact_audio_source' in case:
        actual = described_aiff['s32'] if described_aiff is not None else decoded_audio(path)
        expected = decoded_audio(FIXTURES / case['exact_audio_source'])
        assert actual and actual == expected, f'{path}: lossless encoding changed sample values, speaker order or duration'
    if 'speed_source' in case:
        check_audio_speed(path, case)
    if 'timing_speed_source' in case:
        check_audio_speed_timing(path, case)
    if 'adjusted_audio_source' in case:
        actual = float_samples(decoded_audio(path, 'f64le', 'pcm_f64le'))
        original = float_samples(decoded_audio(FIXTURES / case['adjusted_audio_source'], 'f64le', 'pcm_f64le'))
        assert len(actual) == len(original) > 0, f'{path}: gain/fades or limiter lookahead changed sample count'
        gain = 10 ** (case['gain_centidb'] / 2000)
        if case['gain_centidb'] == 2400:
            peak = max(abs(value) for value in actual)
            assert 0.8 < peak <= 0.95005, f'{path}: positive gain did not apply bounded peak protection: {peak}'
            assert max(abs(value * gain) for value in original) > 1.5, 'The peak-protection fixture must actually exceed full scale'
        else:
            channels, rate = case['channels'], case['sample_rate']
            frames = len(original) // channels
            fade_in = case['fade_in_us'] * rate / 1_000_000
            fade_out = case['fade_out_us'] * rate / 1_000_000
            def reference(index, value):
                frame = index // channels
                envelope = min(1, frame / fade_in if fade_in else 1,
                               (frames - frame) / fade_out if fade_out else 1)
                return value * gain * max(0, envelope)
            error = max(abs(value - reference(index, original[index])) for index, value in enumerate(actual))
            assert error <= 2 / 32768, f'{path}: decoded gain/fade envelope, timing or channel order differs: {error}'
    if 'pcm_precision_source' in case:
        actual = described_aiff['values'] if described_aiff is not None else float_samples(decoded_audio(path, 'f64le', 'pcm_f64le'))
        expected = float_samples(decoded_audio(FIXTURES / case['pcm_precision_source'], 'f64le', 'pcm_f64le'))
        assert len(actual) == len(expected) > 0, f'{path}: PCM conversion changed sample count or channel count'
        tolerance = 2.0 ** (1 - case['pcm_quantization_bits'])
        assert max(abs(a - b) for a, b in zip(actual, expected)) <= tolerance, f'{path}: PCM samples exceed explicit quantization error or channels were reordered'
    if 'ten_bit_reference' in case:
        def decoded_ten_bit(media):
            raw = run(['ffmpeg', '-v', 'error', '-nostdin', '-threads', '1', '-i', str(media),
                       '-map', '0:v:0', '-pix_fmt', 'yuv420p10le', '-f', 'rawvideo', '-'])
            values = array.array('H')
            values.frombytes(raw)
            if sys.byteorder != 'little':
                values.byteswap()
            return values
        actual = decoded_ten_bit(path)
        expected = decoded_ten_bit(FIXTURES / case['ten_bit_reference'])
        assert len(actual) == len(expected) == case['width'] * case['height'] * 3 // 2 * 12, f'{path}: ten-bit frame content/count changed'
        assert any(value % 4 for value in expected), 'Independent ten-bit fixture lacks low-bit content'
        assert any(value % 4 for value in actual), f'{path}: decoded output contains only promoted eight-bit samples'
        error = math.sqrt(sum((a - b) ** 2 for a, b in zip(actual, expected)) / len(actual))
        assert error < 32, f'{path}: ten-bit decoded content differs from source: RMSE={error}'
    if case.get('hdr10_elementary_sei'):
        # Probe decoded elementary HEVC, excluding all MP4/TS container-side
        # metadata. These side data must come from actual encoded SEI NAL units.
        coded_pictures = []
        for media in [FIXTURES / case['source'], path]:
            elementary = run(['ffmpeg', '-v', 'error', '-nostdin', '-i', str(media), '-map', '0:v:0',
                              '-c:v', 'copy', '-bsf:v', 'hevc_mp4toannexb', '-f', 'hevc', '-'])
            vcl = [nal for nal in re.split(b'\x00\x00(?:\x00)?\x01', elementary) if len(nal) >= 2 and ((nal[0] >> 1) & 63) <= 31]
            assert len(vcl) >= 12, f'{media}: missing HEVC coded pictures'
            coded_pictures.append(vcl)
            decoded = subprocess.run(['ffprobe', '-v', 'error', '-f', 'hevc', '-show_frames', '-of', 'json', '-'],
                                     input=elementary, stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=True)
            frames = json.loads(decoded.stdout)['frames']
            sides = [side for frame in frames for side in frame.get('side_data_list', [])]
            mastering = [side for side in sides if side['side_data_type'] == 'Mastering display metadata']
            light = [side for side in sides if side['side_data_type'] == 'Content light level metadata']
            assert len(frames) == 12 and mastering and light, f'{media}: missing decoded HDR10 elementary SEI'
            expected_mastering = {'red_x': '34000/50000', 'red_y': '16000/50000',
                                  'green_x': '13250/50000', 'green_y': '34500/50000',
                                  'blue_x': '7500/50000', 'blue_y': '3000/50000',
                                  'white_point_x': '15635/50000', 'white_point_y': '16450/50000',
                                  'max_luminance': '10000000/10000', 'min_luminance': '50/10000'}
            for entry in mastering:
                assert all(Fraction(entry.get(key, '0')) == Fraction(value) for key, value in expected_mastering.items()), f'{media}: mastering SEI values changed'
            assert all(entry.get('max_content') == 1000 and entry.get('max_average') == 400 for entry in light), f'{media}: MaxCLL/MaxFALL SEI changed'
        assert coded_pictures[0] != coded_pictures[1], f'{path}: HEVC frames were only remuxed instead of re-encoded'
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
        if "attached_picture_codec" in case:
            original_pictures = [stream for stream in source["streams"] if stream.get("disposition", {}).get("attached_pic")]
            assert len(original_pictures) == 1, f"{path}: missing independent source artwork"
            before = [packet["data_hash"] for packet in source["packets"] if packet["stream_index"] == original_pictures[0]["index"]]
            after = [packet["data_hash"] for packet in info["packets"] if packet["stream_index"] == pictures[0]["index"]]
            assert len(before) == 1 and before == after, f"{path}: copied artwork payload changed"
            assert first_rgb(path, 32, 32) == first_rgb(FIXTURES / case["source"], 32, 32), f"{path}: decoded artwork changed"
        if case.get('preserve_audio_packets'):
            before = [packet['data_hash'] for packet in media_packets(source, 'audio')]
            after = [packet['data_hash'] for packet in media_packets(info, 'audio')]
            assert before and before == after, f'{path}: Original/Automatic changed compatible encoded audio payloads'
        if case.get("preserve_video_packets"):
            original = media_packets(source, "video")
            result = media_packets(info, "video")
            assert len(original) == len(result), f"{path}: changed copied frame/packet count"
            assert [packet["data_hash"] for packet in original] == [packet["data_hash"] for packet in result], f"{path}: copied compressed video payload changed"
            shift = float(result[0]["pts_time"]) - float(original[0]["pts_time"])
            for before, after in zip(original, result):
                assert_close(float(after["pts_time"]) - shift, float(before["pts_time"]), 0.002, f"{path}: copied presentation timestamp")
        if case.get("reencoded_video"):
            original = media_packets(source, "video")
            result = media_packets(info, "video")
            assert len(original) == len(result) > 0, f"{path}: video encoding changed frame count"
            assert [packet["data_hash"] for packet in original] != [packet["data_hash"] for packet in result], f"{path}: explicit encoding only copied compressed video"
        if "decoded_video_frames" in case:
            frames = json.loads(run(["ffprobe", "-v", "error", "-select_streams", "v:0", "-show_frames", "-of", "json", str(path)]))
            assert len(frames.get("frames", [])) == case["decoded_video_frames"], f"{path}: independently decoded frame count changed"
        if case.get("match_source_frame"):
            actual = first_rgb(path, case["width"], case["height"])
            original = first_rgb(FIXTURES / case["source"], case["width"], case["height"])
            assert rgb_error(actual, original) < 15, f"{path}: encoded picture does not match the selected source"
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
        if case.get("preserve_auxiliary_tracks_and_chapters"):
            assert [(stream["codec_type"], stream["codec_name"]) for stream in info["streams"]] == [
                (stream["codec_type"], stream["codec_name"]) for stream in source["streams"]], f"{path}: preserved track identities changed"
            shift = float(media_packets(info, "video")[0]["pts_time"]) - float(media_packets(source, "video")[0]["pts_time"])
            for original, result in zip(source["streams"], info["streams"]):
                for key in ["language", "title"]:
                    assert result.get("tags", {}).get(key) == original.get("tags", {}).get(key), f"{path}: track {key} changed"
                for key in ["default", "forced", "hearing_impaired", "visual_impaired"]:
                    assert result.get("disposition", {}).get(key) == original.get("disposition", {}).get(key), f"{path}: disposition {key} changed"
                if original["codec_type"] in {"audio", "subtitle"}:
                    before = [packet for packet in source["packets"] if packet["stream_index"] == original["index"]]
                    after = [packet for packet in info["packets"] if packet["stream_index"] == result["index"]]
                    assert len(before) == len(after) > 0, f"{path}: copied auxiliary packet count changed"
                    assert [packet["data_hash"] for packet in before] == [packet["data_hash"] for packet in after], f"{path}: copied auxiliary payload changed"
                    for old, new in zip(before, after):
                        assert_close(float(new["pts_time"]) - shift, float(old["pts_time"]), 0.002, f"{path}: auxiliary timing changed")
            assert len(info["chapters"]) == len(source["chapters"]) == 2, f"{path}: chapters lost"
            for original, result in zip(source["chapters"], info["chapters"]):
                assert original.get("tags") == result.get("tags"), f"{path}: chapter metadata changed"
                for key in ["start_time", "end_time"]:
                    assert_close(float(result[key]) - shift, float(original[key]), 0.002, f"{path}: chapter {key} changed")
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
            try:
                checked.append(check_case(case))
            except AssertionError as error:
                raise AssertionError(f"{manifest}: {case.get('path', '<missing output>')}: {error}") from error
    assert len(checked) >= 8, f"Incomplete conversion output set: {checked}"
    result = {"verified_outputs": checked, "fixture_tool": fixture_manifest["ffmpeg_version"],
              "verification_tool": run(["ffprobe", "-version"]).decode().splitlines()[0],
              "original_hashes_unchanged": len(fixture_manifest["files"]),
              "aiff_speaker_evidence": AIFF_LAYOUT_EVIDENCE}
    (OUTPUTS / "independent-verification.json").write_text(json.dumps(result, indent=2) + "\n")
    print(f"Independently inspected and decoded {len(checked)} real outputs; original hashes unchanged")


if __name__ == "__main__":
    main()
