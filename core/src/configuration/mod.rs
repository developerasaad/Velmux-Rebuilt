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
    _source: &MediaInfo,
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
    ConversionConfig {
        operation,
        format: if operation == Operation::VideoConverter {
            OutputFormat::Mp4
        } else {
            OutputFormat::M4a
        },
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
    }
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct FormatOption {
    pub format: OutputFormat,
    pub label: String,
    pub extension: String,
    pub video_codecs: Vec<VideoCodec>,
    pub audio_codecs: Vec<AudioCodec>,
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
    catalog(operation).into_iter().filter_map(|mut option|{
        let name=std::ffi::CString::new(option.format.muxer()).unwrap();
        let muxer=unsafe{sys::av_guess_format(name.as_ptr(),std::ptr::null(),std::ptr::null())};
        if muxer.is_null(){return None;}
        let compatible=|kind:StreamKind| info.streams.iter().filter(|stream|stream.kind==kind).all(|stream|unsafe{sys::avformat_query_codec(muxer,stream.codec_id as _,0)}>0);
        if !compatible(StreamKind::Video){option.video_codecs.retain(|codec|*codec!=VideoCodec::Copy);}
        if !compatible(StreamKind::Audio){option.audio_codecs.retain(|codec|*codec!=AudioCodec::Copy);}
        Some(option)
    }).collect()
}
