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
run_ffmpeg -f lavfi -i 'aevalsrc=0.15*sin(2*PI*440*t)|0.15*sin(2*PI*660*t):s=48000:d=2' \
  -c:a pcm_s16le -map_metadata -1 "$fixtures/stereo.wav"
run_ffmpeg -i "$fixtures/stereo.wav" -c:a flac -compression_level 5 "$fixtures/stereo.flac"
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
run_ffmpeg -f lavfi -i 'testsrc2=size=160x90:rate=12:duration=1' \
  -vf format=yuv420p10le -c:v ffv1 -level 3 -threads:v 1 \
  -color_primaries bt2020 -color_trc smpte2084 -colorspace bt2020nc "$fixtures/hdr.mkv"
python3 - "$fixtures" <<'MANIFEST'
import hashlib, json, pathlib, subprocess, sys
root = pathlib.Path(sys.argv[1])
(root / 'invalid.bin').write_bytes(b'not a media file\x00' * 80)
(root / 'truncated.mp4').write_bytes((root / 'video.mp4').read_bytes()[:256])
probes = {}
for path in root.iterdir():
    if path.suffix in {'.mp4', '.mkv', '.wav', '.flac'} and path.name != 'truncated.mp4':
        probes[path.name] = json.loads(subprocess.check_output(
            ['ffprobe', '-v', 'error', '-show_format', '-show_streams', '-show_chapters', '-of', 'json', str(path)], text=True, timeout=15))
(root / 'probes.json').write_text(json.dumps(probes, indent=2) + '\n')
rotation = probes['rotated-sar.mp4']['streams'][0].get('side_data_list', [])
print('Independent rotation facts:', rotation)
assert any(abs(item.get('rotation', 0)) == 90 for item in rotation), 'The independently generated rotation fixture lacks its required display matrix'
files = {path.name: hashlib.sha256(path.read_bytes()).hexdigest()
         for path in root.iterdir() if path.name != 'fixtures.json' and path.is_file()}
manifest = {'files': files, 'ffmpeg_version': subprocess.check_output(['ffmpeg', '-version'], text=True).splitlines()[0]}
(root / 'fixtures.json').write_text(json.dumps(manifest, indent=2) + '\n')
print(f'Prepared {len(files)} short independent media fixtures in {root}')
MANIFEST
