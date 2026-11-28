#!/usr/bin/env bash
# Independent development fixtures; none of these CLI tools are used by the app.
set -euo pipefail
cd "$(dirname "$0")/../.."
if ! command -v ffmpeg >/dev/null || ! command -v ffprobe >/dev/null; then
  if [ "${CI:-}" = true ] && [ "$(uname -s)" = Linux ]; then
    sudo apt-get update -qq
    sudo apt-get install -y ffmpeg
  elif [ "${CI:-}" = true ] && [ "$(uname -s)" = Darwin ]; then
    brew install ffmpeg
  else
    echo 'Host media tests require independent ffmpeg and ffprobe development tools.' >&2
    exit 1
  fi
fi
fixtures="$PWD/build/media-fixtures"
mkdir -p "$fixtures" "$PWD/build/media-test-output"
run_ffmpeg() { ffmpeg -hide_banner -loglevel error -nostdin -y -threads 1 "$@"; }
run_ffmpeg -f lavfi -i 'testsrc2=size=320x180:rate=24000/1001:duration=2' \
  -f lavfi -i 'aevalsrc=0.15*sin(2*PI*440*t)|0.15*sin(2*PI*660*t):s=48000:d=2' \
  -map 0:v -map 1:a -c:v libx264 -preset ultrafast -pix_fmt yuv420p -threads:v 1 \
  -c:a aac -b:a 192k -map_metadata -1 "$fixtures/video.mp4"
# Asymmetric corner colors expose incorrect rotation/reflection composition.
run_ffmpeg -f lavfi -i 'color=red:size=160x96:rate=12:duration=1,drawbox=x=80:y=0:w=80:h=48:color=lime:t=fill,drawbox=x=0:y=48:w=80:h=48:color=blue:t=fill,drawbox=x=80:y=48:w=80:h=48:color=yellow:t=fill' \
  -c:v libx264 -preset ultrafast -crf 12 -pix_fmt yuv420p -threads:v 1 \
  -map_metadata -1 "$fixtures/orientation-grid.mp4"
run_ffmpeg -i "$fixtures/video.mp4" -vf transpose=clock -c:v libx264 \
  -preset ultrafast -threads:v 1 -c:a copy "$fixtures/portrait.mp4"
run_ffmpeg -f lavfi -i 'aevalsrc=0.15*sin(2*PI*440*t)|0.15*sin(2*PI*660*t):s=48000:d=2' \
  -c:a pcm_s16le -map_metadata -1 "$fixtures/stereo.wav"
run_ffmpeg -i "$fixtures/stereo.wav" -c:a flac -compression_level 5 "$fixtures/stereo.flac"
# Actual level, dynamics and gating cases for the Rust-owned loudness stage.
for amplitude in 0.02 0.60; do
  label=quiet
  [ "$amplitude" != 0.60 ] || label=loud
  run_ffmpeg -f lavfi -i "aevalsrc=$amplitude*sin(2*PI*440*t)|$amplitude*sin(2*PI*660*t):s=48000:d=8" \
    -c:a pcm_s16le "$fixtures/normalization-$label.wav"
done
run_ffmpeg -f lavfi -i 'aevalsrc=if(lt(t\,4)\,0.02\,if(lt(t\,8)\,0.3\,0.07))*sin(2*PI*440*t)|if(lt(t\,4)\,0.02\,if(lt(t\,8)\,0.3\,0.07))*sin(2*PI*660*t):s=48000:d=12' \
  -c:a pcm_s16le "$fixtures/normalization-modulated.wav"
run_ffmpeg -f lavfi -i 'anullsrc=r=48000:cl=stereo:d=2' \
  -c:a pcm_f64le "$fixtures/normalization-silence.wav"
run_ffmpeg -f lavfi -i 'anullsrc=r=48000:cl=5.1:d=0.417' \
  -c:a pcm_f64le "$fixtures/normalization-short-silence.wav"
run_ffmpeg -f lavfi -i 'aevalsrc=0.04*sin(2*PI*440*t)|0.04*sin(2*PI*660*t):s=48000:d=0.217' \
  -c:a pcm_s16le "$fixtures/normalization-short.wav"
run_ffmpeg -f lavfi -i 'aevalsrc=0.04*sin(2*PI*440*t)|0.04*sin(2*PI*660*t):s=48000:d=0.617' \
  -c:a pcm_s16le "$fixtures/normalization-fractional.wav"
run_ffmpeg -f lavfi -i 'aevalsrc=0.000001*sin(2*PI*440*t)|0.000001*sin(2*PI*660*t):s=48000:d=2' \
  -c:a pcm_f64le "$fixtures/normalization-below-gate.wav"
# PCM fixtures exercise fractional playback-rate arithmetic and real timestamp
# gaps without an intervening lossy codec. MOV preserves per-track durations.
run_ffmpeg -f lavfi -i 'aevalsrc=0.15*sin(2*PI*440*t):s=11025:d=2' \
  -c:a pcm_s16le "$fixtures/odd-rate.wav"
run_ffmpeg -itsoffset 5 -i "$fixtures/stereo.wav" \
  -map 0:a -c:a pcm_s16le -copyts -avoid_negative_ts disabled "$fixtures/audio-origin.mov"
run_ffmpeg -i "$fixtures/stereo.wav" -af 'asetpts=PTS+2*gte(T\,1)/TB' \
  -c:a pcm_s16le -avoid_negative_ts disabled "$fixtures/audio-gap.mkv"
# A tiny packet file with a long timeline gap exercises cooperative cancellation
# during real bounded silence feeding/WSOLA analysis, without a large fixture.
run_ffmpeg -i "$fixtures/stereo.wav" -af 'asetpts=PTS+3600*gte(T\,1)/TB' \
  -c:a pcm_s16le -avoid_negative_ts disabled "$fixtures/audio-long-gap.mkv"
run_ffmpeg -f lavfi -i 'color=c=0xb87333:size=32x32:duration=0.1' \
  -frames:v 1 -c:v png -threads:v 1 -update 1 "$fixtures/cover.png"
run_ffmpeg -i "$fixtures/stereo.flac" -i "$fixtures/cover.png" \
  -map 0:a -map 1:v -c copy -disposition:v:0 attached_pic \
  -metadata:s:v:0 title='Embedded copper cover' -metadata:s:v:0 comment='Cover (front)' \
  "$fixtures/cover-art.flac"
run_ffmpeg -f lavfi -i 'aevalsrc=0.15*sin(2*PI*440*t)|0.15*sin(2*PI*660*t):s=192000:d=2' \
  -c:a flac "$fixtures/high-rate.flac"
run_ffmpeg -f lavfi -i 'aevalsrc=0.1*sin(2*PI*440*t)|0.1*sin(2*PI*660*t)|0.1*sin(2*PI*880*t)|0|0.1*sin(2*PI*440*t)|0.1*sin(2*PI*660*t):s=48000:d=2:c=5.1' \
  -c:a flac "$fixtures/surround.flac"
run_ffmpeg -f lavfi -i 'sine=frequency=440:sample_rate=8000:duration=2' -c:a flac "$fixtures/low-rate.flac"
# Coded MPEG-2.5 duration includes priming/padding; decoded audio is exactly 2 s.
run_ffmpeg -i "$fixtures/low-rate.flac" -c:a libmp3lame -b:a 64000 "$fixtures/gapless-8k.mp3"
run_ffmpeg -i "$fixtures/stereo.wav" -c:a pcm_s32le "$fixtures/pcm32.wav"
# Distinct speaker tones and nonzero low sample bits expose channel swaps,
# precision truncation and accidental mixing in lossless conversion routes.
run_ffmpeg -f lavfi -i 'aevalsrc=0.1*sin(2*PI*220*t)|0.1*sin(2*PI*330*t)|0.1*sin(2*PI*440*t)|0.1*sin(2*PI*55*t)|0.1*sin(2*PI*660*t)|0.1*sin(2*PI*880*t):s=48000:d=2:c=5.1' \
  -c:a pcm_s24le -map_metadata -1 "$fixtures/precision24.wav"
# Generate genuine ten-bit samples, including low bits that an eight-bit
# test pattern promoted to ten-bit would never exercise.
python3 - "$fixtures/precision10.yuv" <<'TEN_BIT_SAMPLES'
import array, sys
with open(sys.argv[1], 'wb') as output:
    for frame in range(12):
        for plane, width, height in [(0, 160, 90), (1, 80, 45), (2, 80, 45)]:
            values = array.array('H', (
                64 + (x * 7 + y * 11 + frame * 13) % 877 if plane == 0
                else 384 + (x * 3 + y * 5 + frame * 7 + plane * 31) % 256
                for y in range(height) for x in range(width)))
            if sys.byteorder != 'little':
                values.byteswap()
            values.tofile(output)
TEN_BIT_SAMPLES
run_ffmpeg -f rawvideo -pixel_format yuv420p10le -video_size 160x90 -framerate 12 \
  -i "$fixtures/precision10.yuv" -c:v ffv1 -level 3 -threads:v 1 \
  -color_primaries bt709 -color_trc bt709 -colorspace bt709 -color_range tv "$fixtures/precision10.mkv"
run_ffmpeg -i "$fixtures/precision10.mkv" -c:v libx265 -preset ultrafast -crf 18 \
  -x265-params 'pools=1:frame-threads=1:log-level=error' -pix_fmt yuv420p10le \
  -color_primaries bt709 -color_trc bt709 -colorspace bt709 -color_range tv "$fixtures/camera10.mp4"
# HDR10 static metadata is encoded as HEVC SEI, independently of container tags.
run_ffmpeg -i "$fixtures/precision10.mkv" -c:v libx265 -preset ultrafast -crf 18 \
  -x265-params 'pools=1:frame-threads=1:log-level=error:hdr10=1:master-display=G(13250,34500)B(7500,3000)R(34000,16000)WP(15635,16450)L(10000000,50):max-cll=1000,400' \
  -pix_fmt yuv420p10le -color_primaries bt2020 -color_trc smpte2084 \
  -colorspace bt2020nc -color_range tv "$fixtures/camera10-hdr.mp4"
run_ffmpeg -f lavfi -i 'testsrc=size=321x181:rate=12:duration=1' \
  -vf format=yuv444p -c:v ffv1 -threads:v 1 "$fixtures/odd-video.mkv"
run_ffmpeg -f lavfi -i 'testsrc2=size=960x540:rate=24:duration=1' -c:v libx264 \
  -preset ultrafast -threads:v 1 -pix_fmt yuv420p "$fixtures/video-large.mp4"
run_ffmpeg -f lavfi -i 'testsrc2=size=160x90:rate=12:duration=40' \
  -f lavfi -i 'aevalsrc=0.12*sin(2*PI*if(lt(t\,20)\,440\,880)*t)|0.12*sin(2*PI*if(lt(t\,20)\,660\,990)*t):s=48000:d=40' \
  -map 0:v -map 1:a -c:v libx264 -preset ultrafast -crf 28 -threads:v 1 \
  -pix_fmt yuv420p -c:a aac -b:a 96k "$fixtures/long-preview.mp4"
run_ffmpeg -itsoffset 5 -i "$fixtures/video.mp4" -itsoffset 5.25 -i "$fixtures/video.mp4" \
  -map 0:v -map 1:a -c copy -copyts -avoid_negative_ts disabled "$fixtures/positive-origin.mkv"
run_ffmpeg -f lavfi -i 'testsrc2=size=320x180:rate=30:duration=3' \
  -vf "select='if(lt(t,1),not(mod(n,2)),not(mod(n,3)))'" -fps_mode vfr \
  -c:v libx264 -preset ultrafast -threads:v 1 -pix_fmt yuv420p "$fixtures/vfr.mkv"
run_ffmpeg -i "$fixtures/video.mp4" -an -vf setsar=4/3 -c:v libx264 \
  -preset ultrafast -threads:v 1 "$fixtures/sar-intermediate.mp4"
run_ffmpeg -display_rotation:v:0 90 -i "$fixtures/sar-intermediate.mp4" -c copy "$fixtures/rotated-sar.mp4"
rm "$fixtures/sar-intermediate.mp4"
cat > "$fixtures/captions.srt" <<'SUBTITLES'
1
00:00:00,250 --> 00:00:01,500
Velmux fixture — English
SUBTITLES
run_ffmpeg -i "$fixtures/video.mp4" -i "$fixtures/stereo.wav" -i "$fixtures/captions.srt" \
  -map 0:v -map 0:a -map 1:a -map 2:s -c:v copy -c:a aac -c:s srt \
  -metadata:s:a:0 language=eng -metadata:s:a:1 language=fra \
  -metadata:s:a:0 title='First tone track' -metadata:s:a:1 title='Second tone track' \
  "$fixtures/multi-track.mkv"
cat > "$fixtures/chapters.txt" <<'CHAPTERS'
;FFMETADATA1
[CHAPTER]
TIMEBASE=1/1000
START=0
END=1000
title=Opening
[CHAPTER]
TIMEBASE=1/1000
START=1000
END=2000
title=Closing
CHAPTERS
run_ffmpeg -i "$fixtures/multi-track.mkv" -f ffmetadata -i "$fixtures/chapters.txt" \
  -map 0 -map_metadata 0 -map_chapters 1 -c copy "$fixtures/chaptered.mkv"
for transfer in smpte2084 arib-std-b67; do
  name=hdr
  [ "$transfer" != arib-std-b67 ] || name=hlg
  run_ffmpeg -f rawvideo -pixel_format yuv420p10le -video_size 160x90 -framerate 12 \
    -i "$fixtures/precision10.yuv" -c:v ffv1 -level 3 -threads:v 1 \
    -color_primaries bt2020 -color_trc "$transfer" -colorspace bt2020nc -color_range tv "$fixtures/$name.mkv"
done
python3 - "$fixtures" <<'MANIFEST'
import hashlib, json, pathlib, subprocess, sys
root = pathlib.Path(sys.argv[1])
# Verify actual decoded content before using the grid as a spatial oracle.
grid = subprocess.check_output(['ffmpeg', '-v', 'error', '-nostdin', '-noautorotate',
                                '-i', str(root / 'orientation-grid.mp4'), '-pix_fmt', 'rgb24', '-f', 'rawvideo', '-'])
frame_bytes = 160 * 96 * 3
assert len(grid) == frame_bytes * 12, 'Spatial fixture must contain twelve real 160x96 frames'
for offset in range(0, len(grid), frame_bytes):
    for (x, y), expected in zip([(40,24),(120,24),(40,72),(120,72)], [(1,0,0),(0,1,0),(0,0,1),(1,1,0)]):
        pixel = grid[offset + (y * 160 + x) * 3:offset + (y * 160 + x) * 3 + 3]
        assert all(value > 200 if high else value < 40 for value, high in zip(pixel, expected)), 'Spatial fixture corner colors changed'
(root / 'invalid.bin').write_bytes(b'not a media file\x00' * 80)
(root / 'truncated.mp4').write_bytes((root / 'video.mp4').read_bytes()[:256])
probes = {}
for path in root.iterdir():
    if path.suffix in {'.mp4', '.mkv', '.mov', '.wav', '.flac', '.mp3'} and path.name != 'truncated.mp4':
        probes[path.name] = json.loads(subprocess.check_output(
            ['ffprobe', '-v', 'error', '-show_format', '-show_streams', '-show_chapters', '-of', 'json', str(path)], text=True, timeout=15))
(root / 'probes.json').write_text(json.dumps(probes, indent=2) + '\n')
gapless_packets = json.loads(subprocess.check_output(['ffprobe', '-v', 'error', '-show_packets', '-of', 'json', str(root / 'gapless-8k.mp3')], text=True))['packets']
assert sum(float(packet['duration_time']) for packet in gapless_packets) > 2.1, 'Gapless fixture lost its coded padding interval'
gapless_samples = subprocess.check_output(['ffmpeg', '-v', 'error', '-nostdin', '-i', str(root / 'gapless-8k.mp3'), '-f', 's16le', '-acodec', 'pcm_s16le', '-'])
assert len(gapless_samples) == 16000 * 2, 'Gapless fixture must decode to exactly two seconds'
precision = probes['precision24.wav']['streams'][0]
assert precision['channels'] == 6 and precision['channel_layout'] == '5.1' and int(precision['bits_per_sample']) == 24
for name in ['precision10.mkv', 'camera10.mp4', 'camera10-hdr.mp4', 'hdr.mkv', 'hlg.mkv']:
    assert probes[name]['streams'][0]['pix_fmt'] == 'yuv420p10le', f'{name}: fixture lost required ten-bit samples'
assert probes['camera10.mp4']['streams'][0]['codec_name'] == 'hevc'
assert probes['camera10.mp4']['streams'][0]['profile'] == 'Main 10'
hdr_frames = json.loads(subprocess.check_output(['ffprobe', '-v', 'error', '-select_streams', 'v:0',
    '-show_frames', '-of', 'json', str(root / 'camera10-hdr.mp4')], text=True, timeout=15))['frames']
hdr_sides = [side for frame in hdr_frames for side in frame.get('side_data_list', [])]
assert any(side['side_data_type'] == 'Mastering display metadata' for side in hdr_sides), 'Independent HEVC fixture lacks mastering SEI'
assert any(side['side_data_type'] == 'Content light level metadata' and side.get('max_content') == 1000 and side.get('max_average') == 400 for side in hdr_sides), 'Independent HEVC fixture lacks content-light SEI'


rotation = probes['rotated-sar.mp4']['streams'][0].get('side_data_list', [])
print('Independent rotation facts:', rotation)
assert any(abs(item.get('rotation', 0)) == 90 for item in rotation), 'The independently generated rotation fixture lacks its required display matrix'
artwork = [stream for stream in probes['cover-art.flac']['streams'] if stream.get('disposition', {}).get('attached_pic') == 1]
assert len(artwork) == 1 and artwork[0]['codec_name'] == 'png' and (artwork[0]['width'], artwork[0]['height']) == (32, 32), 'The independently generated FLAC fixture lacks its required PNG attached picture'
files = {path.name: hashlib.sha256(path.read_bytes()).hexdigest()
         for path in root.iterdir() if path.name != 'fixtures.json' and path.is_file()}
manifest = {'files': files, 'ffmpeg_version': subprocess.check_output(['ffmpeg', '-version'], text=True).splitlines()[0]}
(root / 'fixtures.json').write_text(json.dumps(manifest, indent=2) + '\n')
print(f'Prepared {len(files)} short independent media fixtures in {root}')
MANIFEST
