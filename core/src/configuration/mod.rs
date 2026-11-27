use crate::{EngineError, MediaInfo};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, uniffi::Enum)]
pub enum Operation {
    VideoConverter,
    AudioConverter,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, uniffi::Enum)]
pub enum OutputFormat {
    Mp4,
    Matroska,
    M4a,
    Wav,
    Flac,
    Mp3,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, uniffi::Enum)]
pub enum VideoCodec {
    Automatic,
    H264,
    Copy,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, uniffi::Enum)]
pub enum AudioCodec {
    Automatic,
    Aac,
    Mp3,
    PcmS16le,
    Flac,
    Copy,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, uniffi::Enum)]
pub enum VideoSize {
    Original,
    Hd720,
    Sd480,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, uniffi::Enum)]
pub enum FrameRate {
    Original,
    Fps30,
}

#[derive(Clone, Debug, Serialize, Deserialize, uniffi::Record)]
pub struct ConversionConfig {
    pub operation: Operation,
    pub format: OutputFormat,
    pub video_codec: VideoCodec,
    pub audio_codec: AudioCodec,
    pub video_size: VideoSize,
    pub frame_rate: FrameRate,
    pub quality: u8,
    pub audio_bitrate: Option<u32>,
    pub sample_rate: Option<u32>,
    pub channels: Option<u32>,
    pub file_name: String,
}

impl OutputFormat {
    pub(crate) fn muxer(self) -> &'static str {
        match self {
            Self::Mp4 => "mp4",
            Self::Matroska => "matroska",
            Self::M4a => "ipod",
            Self::Wav => "wav",
            Self::Flac => "flac",
            Self::Mp3 => "mp3",
        }
    }
    pub fn extension(self) -> String {
        match self {
            Self::Mp4 => "mp4",
            Self::Matroska => "mkv",
            Self::M4a => "m4a",
            Self::Wav => "wav",
            Self::Flac => "flac",
            Self::Mp3 => "mp3",
        }
        .into()
    }
    pub fn mime_type(self) -> String {
        match self {
            Self::Mp4 => "video/mp4",
            Self::Matroska => "video/x-matroska",
            Self::M4a => "audio/mp4",
            Self::Wav => "audio/wav",
            Self::Flac => "audio/flac",
            Self::Mp3 => "audio/mpeg",
        }
        .into()
    }
    pub(crate) fn is_video(self) -> bool {
        matches!(self, Self::Mp4 | Self::Matroska)
    }
}

pub(crate) fn invalid(detail: impl Into<String>) -> EngineError {
    EngineError::Configuration {
        detail: detail.into(),
    }
}

/// This validates representation-level intent without probing, native discovery,
/// or constructing a processing pipeline. Final submission additionally checks
/// the actual source streams and bundled codec support.
pub(crate) fn validate_fields(config: &ConversionConfig) -> Result<(), EngineError> {
    if (config.operation == Operation::VideoConverter) != config.format.is_video() {
        return Err(invalid("Choose a format for the selected converter."));
    }
    if config.file_name.trim().is_empty()
        || config.file_name.trim() != config.file_name
        || config.file_name.len() > 180
        || config
            .file_name
            .chars()
            .any(|c| c.is_control() || "/\\:*?\"<>|".contains(c))
        || config.file_name.starts_with('.')
        || config.file_name.ends_with('.')
    {
        return Err(invalid(
            "Use a filename of 1–180 bytes without path separators or control characters.",
        ));
    }
    if !(18..=28).contains(&config.quality) {
        return Err(invalid("Video quality must be between 18 and 28."));
    }
    if config
        .audio_bitrate
        .is_some_and(|rate| ![128_000, 192_000, 256_000].contains(&rate))
    {
        return Err(invalid("Choose a supported audio bitrate."));
    }
    if config
        .sample_rate
        .is_some_and(|rate| ![44_100, 48_000].contains(&rate))
    {
        return Err(invalid("Choose Original, 44.1 kHz or 48 kHz."));
    }
    if config
        .channels
        .is_some_and(|count| ![1, 2].contains(&count))
    {
        return Err(invalid("Choose Original, Mono or Stereo."));
    }
    if config.operation == Operation::AudioConverter {
        if config.video_codec != VideoCodec::Automatic
            || config.video_size != VideoSize::Original
            || config.frame_rate != FrameRate::Original
            || config.quality != 23
        {
            return Err(invalid("Video settings do not apply to audio conversion."));
        }
        let expected = match config.format {
            OutputFormat::M4a => AudioCodec::Aac,
            OutputFormat::Wav => AudioCodec::PcmS16le,
            OutputFormat::Flac => AudioCodec::Flac,
            OutputFormat::Mp3 => AudioCodec::Mp3,
            _ => unreachable!(),
        };
        if config.audio_codec != AudioCodec::Automatic
            && config.audio_codec != expected
            && config.audio_codec != AudioCodec::Copy
        {
            return Err(invalid(
                "The selected audio codec is incompatible with this format.",
            ));
        }
        if matches!(config.format, OutputFormat::Wav | OutputFormat::Flac)
            && config.audio_bitrate.is_some()
        {
            return Err(invalid(
                "A lossy bitrate does not apply to this lossless format.",
            ));
        }
    } else if config.audio_codec != AudioCodec::Automatic
        && config.audio_codec != AudioCodec::Copy
        && config.audio_codec != AudioCodec::Aac
    {
        return Err(invalid(
            "This video foundation supports preserved audio or AAC.",
        ));
    }
    if config.video_codec == VideoCodec::Copy
        && (config.frame_rate != FrameRate::Original || config.quality != 23)
    {
        return Err(invalid(
            "Stream copying cannot change video size, timing or encoding quality.",
        ));
    }
    if config.audio_codec == AudioCodec::Copy && config.audio_bitrate.is_some() {
        return Err(invalid(
            "Stream copying cannot change audio sampling, channels or bitrate.",
        ));
    }
    Ok(())
}

pub(crate) fn default_config(
    operation: Operation,
    source: &MediaInfo,
    name: &str,
) -> ConversionConfig {
    let stem = std::path::Path::new(name)
        .file_stem()
        .and_then(|v| v.to_str())
        .unwrap_or("media");
    let mut clean = String::new();
    for c in stem
        .trim()
        .trim_start_matches('.')
        .chars()
        .filter(|c| !c.is_control() && !"/\\:*?\"<>|".contains(*c))
    {
        if clean.len() + c.len_utf8() > 150 {
            break;
        }
        clean.push(c);
    }
    let stem = clean.trim().trim_matches('.');
    let preferred = if operation == Operation::VideoConverter {
        OutputFormat::Mp4
    } else {
        OutputFormat::M4a
    };
    let available = source_catalog(source, operation);
    let format = available
        .iter()
        .find(|option| option.format == preferred)
        .or_else(|| available.first())
        .map_or(preferred, |option| option.format);
    let mut config = ConversionConfig {
        operation,
        format,
        video_codec: VideoCodec::Automatic,
        audio_codec: AudioCodec::Automatic,
        video_size: VideoSize::Original,
        frame_rate: FrameRate::Original,
        quality: 23,
        audio_bitrate: None,
        sample_rate: None,
        channels: None,
        file_name: format!(
            "{}_converted",
            if stem.is_empty() { "media" } else { &stem }
        ),
    };
    let has_audio = source
        .streams
        .iter()
        .any(|stream| stream.kind == crate::StreamKind::Audio);
    let usable = |candidate: &ConversionConfig| {
        let current = choices(source, candidate);
        (!has_audio || current.sample_rates.contains(&0) && current.channels.contains(&0))
            && (operation != Operation::VideoConverter
                || current.video_sizes.contains(&VideoSize::Original))
    };
    if !usable(&config) {
        for option in available
            .iter()
            .filter(|option| option.audio_codecs.contains(&AudioCodec::Copy))
            .chain(
                available
                    .iter()
                    .filter(|option| !option.audio_codecs.contains(&AudioCodec::Copy)),
            )
        {
            config.format = option.format;
            if usable(&config) {
                break;
            }
        }
    }
    config
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct FormatOption {
    pub format: OutputFormat,
    pub label: String,
    pub extension: String,
    pub video_codecs: Vec<VideoCodec>,
    pub audio_codecs: Vec<AudioCodec>,
}

/// Cheap choices for the current intent. Zero denotes the source's original
/// sampling/layout or automatic bitrate; no source probing or pipeline planning.
#[derive(Clone, Debug, uniffi::Record)]
pub struct ConfigurationChoices {
    pub video_sizes: Vec<VideoSize>,
    pub frame_rates: Vec<FrameRate>,
    pub sample_rates: Vec<u32>,
    pub channels: Vec<u32>,
    pub audio_bitrates: Vec<u32>,
}

pub(crate) fn audio_target(format: OutputFormat) -> &'static str {
    match format {
        OutputFormat::Mp3 => "mp3",
        OutputFormat::Wav => "pcm_s16le",
        OutputFormat::Flac => "flac",
        _ => "aac",
    }
}

pub(crate) fn audio_copy_allowed(format: OutputFormat, codec: &str) -> bool {
    match format {
        OutputFormat::M4a => matches!(codec, "aac" | "alac"),
        OutputFormat::Wav => codec == "pcm_s16le",
        OutputFormat::Flac => codec == "flac",
        OutputFormat::Mp3 => codec == "mp3",
        _ => true,
    }
}

pub(crate) fn audio_transform_requested(
    stream: &crate::StreamInfo,
    config: &ConversionConfig,
) -> bool {
    let Some(audio) = &stream.audio else {
        return false;
    };
    config
        .sample_rate
        .is_some_and(|rate| Some(rate) != audio.sample_rate)
        || config.channels.is_some_and(|channels| {
            Some(channels) != audio.channels
                || audio.channel_layout.as_deref()
                    != Some(if channels == 1 { "mono" } else { "stereo" })
        })
        || config.audio_bitrate.is_some()
}

pub(crate) fn scaled_video_dimensions(
    video: &crate::VideoInfo,
    size: VideoSize,
) -> Result<(u32, u32), EngineError> {
    let width = video
        .width
        .filter(|value| *value > 0)
        .ok_or_else(|| invalid("The source width is unknown."))?;
    let height = video
        .height
        .filter(|value| *value > 0)
        .ok_or_else(|| invalid("The source height is unknown."))?;
    if size == VideoSize::Original {
        return Ok((width, height));
    }
    let sar = video.sample_aspect_ratio.as_ref().map_or(1.0, |ratio| {
        f64::from(ratio.numerator) / f64::from(ratio.denominator)
    });
    if !sar.is_finite() || sar <= 0.0 {
        return Err(invalid("The source pixel aspect ratio is invalid."));
    }
    let (long, short) = if size == VideoSize::Hd720 {
        (1280.0, 720.0)
    } else {
        (854.0, 480.0)
    };
    let dw = f64::from(width) * sar;
    let dh = f64::from(height);
    let (bw, bh) = if dw >= dh {
        (long, short)
    } else {
        (short, long)
    };
    let factor = (bw / dw).min(bh / dh).min(1.0);
    Ok(if factor < 1.0 {
        (
            ((f64::from(width) * factor) as u32 / 2) * 2,
            ((f64::from(height) * factor) as u32 / 2) * 2,
        )
    } else {
        (width, height)
    })
}

pub(crate) fn h264_dimensions(
    video: &crate::VideoInfo,
    size: VideoSize,
) -> Result<(u32, u32), EngineError> {
    let dimensions = scaled_video_dimensions(video, size)?;
    if video.width.is_some_and(|width| width > 16384)
        || video.height.is_some_and(|height| height > 16384)
        || video
            .width
            .zip(video.height)
            .is_some_and(|(width, height)| u64::from(width) * u64::from(height) > 64 * 1024 * 1024)
    {
        return Err(invalid(
            "The source geometry exceeds this processing route's limits.",
        ));
    }
    if dimensions.0 < 2 || dimensions.1 < 2 || dimensions.0 % 2 != 0 || dimensions.1 % 2 != 0 {
        return Err(invalid(
            "H.264 requires even dimensions. Choose a supported size or compatible stream copying.",
        ));
    }
    Ok(dimensions)
}

pub(crate) fn audio_encoding_required(
    stream: &crate::StreamInfo,
    config: &ConversionConfig,
    compatible: bool,
) -> bool {
    let transform = audio_transform_requested(stream, config);
    let explicit = !matches!(config.audio_codec, AudioCodec::Automatic | AudioCodec::Copy);
    let target = audio_target(config.format);
    config.audio_codec != AudioCodec::Copy
        && (transform
            || !compatible
            || explicit
                && (stream.codec != target
                    || target == "aac" && stream.profile.as_deref() != Some("LC")))
}

pub(crate) fn audio_rate_supported(target: &str, rate: u32) -> bool {
    match target {
        "aac" => [
            8000, 11025, 12000, 16000, 22050, 24000, 32000, 44100, 48000, 64000, 88200, 96000,
        ]
        .contains(&rate),
        "mp3" => [8000, 11025, 12000, 16000, 22050, 24000, 32000, 44100, 48000].contains(&rate),
        _ => (8000..=192000).contains(&rate),
    }
}

pub(crate) fn audio_bitrate_supported(
    target: &str,
    rate: u32,
    channels: u32,
    bitrate: u32,
) -> bool {
    match target {
        "aac" => u64::from(bitrate) <= u64::from(rate) * u64::from(channels) * 6,
        "mp3" if rate < 16000 => bitrate <= 64000,
        "mp3" if rate < 32000 => bitrate <= 160000,
        "mp3" => true,
        _ => false,
    }
}

pub(crate) fn automatic_audio_bitrate(target: &str, rate: u32, channels: u32) -> u32 {
    match target {
        "aac" => 192000.min(rate.saturating_mul(channels).saturating_mul(6)),
        "mp3" if rate < 16000 => 64000,
        "mp3" if rate < 32000 => 128000,
        _ => 192000,
    }
}

pub(crate) fn choices(info: &MediaInfo, config: &ConversionConfig) -> ConfigurationChoices {
    use crate::{ffmpeg::sys, StreamKind};
    let format = source_catalog(info, config.operation)
        .into_iter()
        .find(|item| item.format == config.format);
    let can_encode_video = format
        .as_ref()
        .is_some_and(|item| item.video_codecs.contains(&VideoCodec::H264))
        && config.video_codec != VideoCodec::Copy
        && info
            .streams
            .iter()
            .filter(|stream| {
                stream.kind == StreamKind::Video
                    && !stream
                        .video
                        .as_ref()
                        .is_some_and(|video| video.attached_picture)
            })
            .all(|stream| stream.decoder_available);
    let muxer_name = std::ffi::CString::new(config.format.muxer()).unwrap();
    let muxer =
        unsafe { sys::av_guess_format(muxer_name.as_ptr(), std::ptr::null(), std::ptr::null()) };
    let audio: Vec<_> = info
        .streams
        .iter()
        .filter(|stream| stream.kind == StreamKind::Audio)
        .collect();
    let target = audio_target(config.format);
    let valid = |candidate: &ConversionConfig,
                 check_rate: bool,
                 check_channels: bool,
                 check_bitrate: bool| {
        !audio.is_empty()
            && !muxer.is_null()
            && audio.iter().all(|stream| {
                let Some(facts) = &stream.audio else {
                    return false;
                };
                let compatible =
                    unsafe { sys::avformat_query_codec(muxer, stream.codec_id as _, 0) } > 0
                        && audio_copy_allowed(candidate.format, &stream.codec);
                if candidate.audio_codec == AudioCodec::Copy {
                    return compatible && !audio_transform_requested(stream, candidate);
                }
                if !audio_encoding_required(stream, candidate, compatible) {
                    return true;
                }
                let rate = candidate.sample_rate.or(facts.sample_rate);
                let channels = candidate.channels.or(facts.channels);
                stream.decoder_available
                    && (target != "flac" || facts.bit_depth.is_none_or(|depth| depth <= 24))
                    && (!check_rate || rate.is_some_and(|rate| audio_rate_supported(target, rate)))
                    && (!check_channels
                        || channels.is_some_and(|channels| {
                            (1..=if target == "mp3" { 2 } else { 8 }).contains(&channels)
                        }))
                    && (!check_bitrate
                        || candidate.audio_bitrate.is_none_or(|bitrate| {
                            rate.zip(channels).is_some_and(|(rate, channels)| {
                                audio_bitrate_supported(target, rate, channels, bitrate)
                            })
                        }))
            })
    };
    let mut result = ConfigurationChoices {
        video_sizes: Vec::new(),
        frame_rates: vec![FrameRate::Original],
        sample_rates: Vec::new(),
        channels: Vec::new(),
        audio_bitrates: Vec::new(),
    };
    if let Some(stream) = info.streams.iter().find(|stream| {
        stream.kind == StreamKind::Video
            && !stream
                .video
                .as_ref()
                .is_some_and(|video| video.attached_picture)
    }) {
        let copy_compatible = !muxer.is_null()
            && unsafe { sys::avformat_query_codec(muxer, stream.codec_id as _, 0) } > 0;
        for size in [VideoSize::Original, VideoSize::Hd720, VideoSize::Sd480] {
            let no_size_change = size == VideoSize::Original
                || stream.video.as_ref().is_some_and(|video| {
                    scaled_video_dimensions(video, size)
                        .ok()
                        .zip(scaled_video_dimensions(video, VideoSize::Original).ok())
                        .is_some_and(|(selected, original)| selected == original)
                });
            if config.video_codec != VideoCodec::H264
                && copy_compatible
                && no_size_change
                && config.frame_rate == FrameRate::Original
                && config.quality == 23
                || can_encode_video
                    && stream
                        .video
                        .as_ref()
                        .is_some_and(|video| h264_dimensions(video, size).is_ok())
            {
                result.video_sizes.push(size);
            }
        }
        if can_encode_video
            && stream
                .video
                .as_ref()
                .is_some_and(|video| h264_dimensions(video, config.video_size).is_ok())
        {
            result.frame_rates.push(FrameRate::Fps30);
        }
    } else {
        result.video_sizes.push(VideoSize::Original);
    }
    for rate in [0, 44100, 48000] {
        let mut candidate = config.clone();
        candidate.sample_rate = (rate != 0).then_some(rate);
        if valid(&candidate, true, false, false) {
            result.sample_rates.push(rate);
        }
    }
    for channels in [0, 1, 2] {
        let mut candidate = config.clone();
        candidate.channels = (channels != 0).then_some(channels);
        if valid(&candidate, false, true, false) {
            result.channels.push(channels);
        }
    }
    if config.audio_codec != AudioCodec::Copy && matches!(target, "aac" | "mp3") {
        for bitrate in [0, 128000, 192000, 256000] {
            let mut candidate = config.clone();
            candidate.audio_bitrate = (bitrate != 0).then_some(bitrate);
            if valid(&candidate, false, false, true) {
                result.audio_bitrates.push(bitrate);
            }
        }
    }
    result
}

pub(crate) fn catalog(operation: Operation) -> Vec<FormatOption> {
    let formats = if operation == Operation::VideoConverter {
        vec![
            (OutputFormat::Mp4, "MP4"),
            (OutputFormat::Matroska, "Matroska"),
        ]
    } else {
        vec![
            (OutputFormat::M4a, "M4A"),
            (OutputFormat::Mp3, "MP3"),
            (OutputFormat::Wav, "WAV"),
            (OutputFormat::Flac, "FLAC"),
        ]
    };
    formats
        .into_iter()
        .map(|(format, label)| FormatOption {
            format,
            label: label.into(),
            extension: format.extension(),
            video_codecs: if format.is_video() {
                vec![VideoCodec::Automatic, VideoCodec::H264, VideoCodec::Copy]
            } else {
                vec![]
            },
            audio_codecs: match format {
                OutputFormat::M4a => vec![AudioCodec::Automatic, AudioCodec::Aac, AudioCodec::Copy],
                OutputFormat::Mp3 => vec![AudioCodec::Automatic, AudioCodec::Mp3, AudioCodec::Copy],
                OutputFormat::Wav => vec![
                    AudioCodec::Automatic,
                    AudioCodec::PcmS16le,
                    AudioCodec::Copy,
                ],
                OutputFormat::Flac => {
                    vec![AudioCodec::Automatic, AudioCodec::Flac, AudioCodec::Copy]
                }
                _ => vec![AudioCodec::Automatic, AudioCodec::Aac, AudioCodec::Copy],
            },
        })
        .collect()
}

pub(crate) fn source_catalog(info: &MediaInfo, operation: Operation) -> Vec<FormatOption> {
    use crate::{ffmpeg::sys, StreamKind};
    catalog(operation)
        .into_iter()
        .filter_map(|mut option| {
            if !info.chapters.is_empty() && option.format != OutputFormat::Matroska {
                return None;
            }
            let name = std::ffi::CString::new(option.format.muxer()).unwrap();
            let muxer =
                unsafe { sys::av_guess_format(name.as_ptr(), std::ptr::null(), std::ptr::null()) };
            if muxer.is_null() {
                return None;
            }
            if info
                .streams
                .iter()
                .filter(|stream| !matches!(stream.kind, StreamKind::Video | StreamKind::Audio))
                .any(|stream| {
                    (unsafe { sys::avformat_query_codec(muxer, stream.codec_id as _, 0) }) <= 0
                })
            {
                return None;
            }
            if info
                .streams
                .iter()
                .filter_map(|stream| stream.video.as_ref())
                .filter(|video| !video.attached_picture)
                .any(|video| {
                    video.bit_depth.is_some_and(|depth| depth > 8)
                        || video.mastering_display.is_some()
                        || video.content_light.is_some()
                        || video
                            .color_transfer
                            .as_deref()
                            .is_some_and(|value| matches!(value, "smpte2084" | "arib-std-b67"))
                })
            {
                option
                    .video_codecs
                    .retain(|codec| *codec != VideoCodec::H264);
                if info
                    .streams
                    .iter()
                    .filter(|stream| stream.kind == StreamKind::Video)
                    .any(|stream| {
                        (unsafe { sys::avformat_query_codec(muxer, stream.codec_id as _, 0) }) <= 0
                    })
                {
                    return None;
                }
            }
            let compatible = |kind: StreamKind| {
                info.streams
                    .iter()
                    .filter(|stream| stream.kind == kind)
                    .all(|stream| {
                        (unsafe { sys::avformat_query_codec(muxer, stream.codec_id as _, 0) }) > 0
                            && (stream.kind != StreamKind::Audio
                                || audio_copy_allowed(option.format, &stream.codec))
                    })
            };
            if option.format == OutputFormat::Flac
                && !compatible(StreamKind::Audio)
                && info
                    .streams
                    .iter()
                    .filter_map(|stream| stream.audio.as_ref())
                    .any(|audio| audio.bit_depth.is_some_and(|depth| depth > 24))
            {
                return None;
            }
            if info
                .streams
                .iter()
                .filter_map(|stream| stream.video.as_ref())
                .filter(|video| !video.attached_picture)
                .any(|video| {
                    [VideoSize::Original, VideoSize::Hd720, VideoSize::Sd480]
                        .into_iter()
                        .all(|size| h264_dimensions(video, size).is_err())
                })
            {
                option
                    .video_codecs
                    .retain(|codec| *codec != VideoCodec::H264);
            }
            if info.streams.iter().any(|stream| {
                stream.kind == StreamKind::Video
                    && !stream
                        .video
                        .as_ref()
                        .is_some_and(|video| video.attached_picture)
                    && !stream.decoder_available
            }) {
                option
                    .video_codecs
                    .retain(|codec| *codec != VideoCodec::H264);
                if !compatible(StreamKind::Video) {
                    return None;
                }
            }
            if info
                .streams
                .iter()
                .any(|stream| stream.kind == StreamKind::Audio && !stream.decoder_available)
            {
                if !compatible(StreamKind::Audio) {
                    return None;
                }
                option
                    .audio_codecs
                    .retain(|codec| matches!(codec, AudioCodec::Automatic | AudioCodec::Copy));
            }
            if !compatible(StreamKind::Video) {
                if option.format.is_video() && !option.video_codecs.contains(&VideoCodec::H264) {
                    return None;
                }
                option
                    .video_codecs
                    .retain(|codec| *codec != VideoCodec::Copy);
            }
            if !compatible(StreamKind::Audio) {
                option
                    .audio_codecs
                    .retain(|codec| *codec != AudioCodec::Copy);
            }
            Some(option)
        })
        .collect()
}
