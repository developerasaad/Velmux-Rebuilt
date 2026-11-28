use crate::{EngineError, MediaInfo};
use serde::{Deserialize, Serialize};

mod audio;
mod catalog;
mod geometry;
pub(crate) use audio::{
    audio_copy_allowed, audio_encoding_required, audio_transform_requested,
    automatic_audio_bitrate, requested_audio_layout, resolve_audio_codec,
};
use audio::{AUDIO_LAYOUTS, AUDIO_PRECISIONS, SAMPLE_RATES};
use catalog::product_formats;
pub(crate) use catalog::{original_format, resolve_format, video_encoding_settings};
pub use catalog::{AudioCodec, AudioLayout, AudioPrecision, OutputFormat, VideoCodec};
mod representation;
pub(crate) use geometry::{
    configured_encoding_dimensions, configured_video_dimensions, scaled_video_dimensions,
};
use geometry::{custom_geometry_defaults, FRAME_RATES, VIDEO_SIZES};
pub use geometry::{FrameRate, VideoSize};
pub use representation::OutputRepresentation;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, uniffi::Enum)]
pub enum Operation {
    VideoConverter,
    AudioConverter,
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
    #[serde(default)]
    pub custom_width: Option<u32>,
    #[serde(default)]
    pub custom_height: Option<u32>,
    #[serde(default)]
    pub custom_frame_rate: Option<crate::Rational>,
    #[serde(default)]
    pub audio_precision: Option<AudioPrecision>,
    #[serde(default)]
    pub channel_layout: Option<AudioLayout>,
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
    if config.format != OutputFormat::Original
        && (config.operation == Operation::VideoConverter) != config.format.is_video()
    {
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
        .is_some_and(|rate| !(500..=6_144_000).contains(&rate))
    {
        return Err(invalid("Choose a supported audio bitrate."));
    }
    if config.sample_rate.is_some_and(|rate| {
        ![
            8000, 11025, 12000, 16000, 22050, 24000, 32000, 44100, 48000, 64000, 88200, 96000,
            176400, 192000,
        ]
        .contains(&rate)
    }) {
        return Err(invalid("Choose a supported sample rate or Original."));
    }
    if config
        .channels
        .is_some_and(|count| ![1, 2, 6, 8].contains(&count))
    {
        return Err(invalid("Choose Original, Mono, Stereo, 5.1 or 7.1."));
    }
    if let Some(layout) = config.channel_layout.and_then(AudioLayout::channels) {
        if config.channels.is_some_and(|channels| channels != layout) {
            return Err(invalid(
                "The selected channel count conflicts with the explicit speaker layout.",
            ));
        }
    }
    config
        .frame_rate
        .rational(config.custom_frame_rate.as_ref())?;
    if config.video_size == VideoSize::Custom {
        let (width, height) = config
            .custom_width
            .zip(config.custom_height)
            .ok_or_else(|| invalid("Enter both custom resolution bounds."))?;
        if !(2..=16384).contains(&width)
            || !(2..=16384).contains(&height)
            || u64::from(width) * u64::from(height) > 64 * 1024 * 1024
        {
            return Err(invalid(
                "Custom bounds must be 2–16384 pixels per side and at most 64 megapixels.",
            ));
        }
    }
    if config.operation == Operation::AudioConverter {
        if config.video_codec != VideoCodec::Automatic
            || config.video_size != VideoSize::Original
            || config.frame_rate != FrameRate::Original
            || config.quality != 23
        {
            return Err(invalid("Video settings do not apply to audio conversion."));
        }
    }
    let definition = config.format.definition();
    if config.format != OutputFormat::Original {
        if config.video_codec != VideoCodec::Automatic
            && config.video_codec != VideoCodec::Copy
            && !definition.video_codecs.contains(&config.video_codec)
        {
            return Err(invalid(
                "The selected video codec is incompatible with this representation.",
            ));
        }
        if config.audio_codec != AudioCodec::Automatic
            && config.audio_codec != AudioCodec::Copy
            && !definition.audio_codecs.contains(&config.audio_codec)
            && !(config.audio_codec == AudioCodec::PcmS16le
                && definition.audio_codecs.contains(&AudioCodec::Pcm))
        {
            return Err(invalid(
                "The selected audio codec is incompatible with this representation.",
            ));
        }
    }
    if !matches!(
        config.video_codec,
        VideoCodec::Automatic | VideoCodec::Copy | VideoCodec::H264
    ) && config.quality != 23
    {
        return Err(invalid("The retained H.264 quality value does not apply to this codec. Use its applicable compression controls."));
    }
    if matches!(
        config.audio_codec,
        AudioCodec::Flac
            | AudioCodec::Alac
            | AudioCodec::Pcm
            | AudioCodec::PcmS16le
            | AudioCodec::Wavpack
    ) && config.audio_bitrate.is_some()
    {
        return Err(invalid(
            "Lossy audio bitrate does not apply to the selected lossless representation.",
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
        .find(|option| option.format == OutputFormat::Original)
        .or_else(|| {
            available.iter().find(|option| {
                (!source.streams.iter().any(primary_video)
                    || option.video_codecs.contains(&VideoCodec::Copy))
                    && (!source
                        .streams
                        .iter()
                        .any(|stream| stream.kind == crate::StreamKind::Audio)
                        || option.audio_codecs.contains(&AudioCodec::Copy))
            })
        })
        .or_else(|| available.iter().find(|option| option.format == preferred))
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
        custom_width: None,
        custom_height: None,
        custom_frame_rate: None,
        audio_precision: None,
        channel_layout: None,
    };
    let has_audio = source
        .streams
        .iter()
        .any(|stream| stream.kind == crate::StreamKind::Audio);
    let usable = |candidate: &ConversionConfig| {
        let current = choices(source, candidate);
        (!has_audio
            || current.sample_rates.contains(&0)
                && current.channels.contains(&0)
                && (current.audio_precisions.is_empty()
                    || current.audio_precisions.contains(&AudioPrecision::Original)))
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
    pub channel_layouts: Vec<AudioLayout>,
    pub audio_precisions: Vec<AudioPrecision>,
    pub default_custom_width: u32,
    pub default_custom_height: u32,
    pub default_custom_frame_rate: crate::Rational,
}

fn primary_video(stream: &crate::StreamInfo) -> bool {
    stream.kind == crate::StreamKind::Video
        && !stream
            .video
            .as_ref()
            .is_some_and(|video| video.attached_picture)
}

/// Muxer/codec declarations are immutable for this linked build. No encoder is
/// opened here, and no pointer is retained outside these narrow library calls.
fn muxer_available(format: OutputFormat) -> bool {
    let Ok(name) = std::ffi::CString::new(format.muxer()) else {
        return false;
    };
    if name.as_bytes().is_empty() {
        return false;
    }
    unsafe {
        !crate::ffmpeg::sys::av_guess_format(name.as_ptr(), std::ptr::null(), std::ptr::null())
            .is_null()
    }
}

fn muxer_accepts(format: OutputFormat, codec: i32) -> bool {
    let Ok(name) = std::ffi::CString::new(format.muxer()) else {
        return false;
    };
    if name.as_bytes().is_empty() {
        return false;
    }
    unsafe {
        let muxer =
            crate::ffmpeg::sys::av_guess_format(name.as_ptr(), std::ptr::null(), std::ptr::null());
        !muxer.is_null() && crate::ffmpeg::sys::avformat_query_codec(muxer, codec as _, 0) > 0
    }
}

fn encoder_accepted(format: OutputFormat, encoder: &str) -> bool {
    if !crate::ffmpeg::encoders::encoder_formats(encoder).is_ok_and(|formats| formats.is_some()) {
        return false;
    }
    let Ok(name) = std::ffi::CString::new(encoder) else {
        return false;
    };
    unsafe {
        let codec = crate::ffmpeg::sys::avcodec_find_encoder_by_name(name.as_ptr());
        !codec.is_null() && muxer_accepts(format, (*codec).id as i32)
    }
}

fn compiled_catalog(operation: Operation) -> Vec<FormatOption> {
    product_formats(operation)
        .into_iter()
        .filter_map(|format| {
            if format == OutputFormat::Original {
                return Some(FormatOption {
                    format,
                    label: "Original".into(),
                    extension: String::new(),
                    video_codecs: if operation == Operation::VideoConverter {
                        vec![VideoCodec::Automatic, VideoCodec::Copy]
                    } else {
                        Vec::new()
                    },
                    audio_codecs: vec![AudioCodec::Automatic, AudioCodec::Copy],
                });
            }
            if !muxer_available(format) {
                return None;
            }
            let definition = format.definition();
            let mut video_codecs = if format.is_video() {
                vec![VideoCodec::Automatic]
            } else {
                Vec::new()
            };
            video_codecs.extend(definition.video_codecs.iter().copied().filter(|codec| {
                codec
                    .encoder()
                    .is_some_and(|encoder| encoder_accepted(format, encoder))
                    || *codec == VideoCodec::H264 && encoder_accepted(format, "libx264rgb")
            }));
            if format.is_video() {
                video_codecs.push(VideoCodec::Copy);
            }
            let mut audio_codecs = vec![AudioCodec::Automatic];
            audio_codecs.extend(definition.audio_codecs.iter().copied().filter(|codec| {
                let encoder = if *codec == AudioCodec::Pcm && format == OutputFormat::Aiff {
                    Some("pcm_s16be")
                } else {
                    codec.encoder()
                };
                encoder.is_some_and(|encoder| encoder_accepted(format, encoder))
            }));
            audio_codecs.push(AudioCodec::Copy);
            Some(FormatOption {
                format,
                label: definition.label.into(),
                extension: format.extension(),
                video_codecs,
                audio_codecs,
            })
        })
        .collect()
}

pub(crate) fn catalog(operation: Operation) -> Vec<FormatOption> {
    use std::sync::LazyLock;
    static VIDEO: LazyLock<Vec<FormatOption>> =
        LazyLock::new(|| compiled_catalog(Operation::VideoConverter));
    static AUDIO: LazyLock<Vec<FormatOption>> =
        LazyLock::new(|| compiled_catalog(Operation::AudioConverter));
    match operation {
        Operation::VideoConverter => VIDEO.clone(),
        Operation::AudioConverter => AUDIO.clone(),
    }
}

fn copy_compatible(format: OutputFormat, stream: &crate::StreamInfo) -> bool {
    muxer_accepts(format, stream.codec_id)
        && (stream.kind != crate::StreamKind::Audio || audio_copy_allowed(format, &stream.codec))
}

fn source_option(
    info: &MediaInfo,
    operation: Operation,
    requested: OutputFormat,
) -> Option<FormatOption> {
    use crate::StreamKind;
    let format = if requested == OutputFormat::Original {
        original_format(info, operation)?
    } else {
        requested
    };
    let mut option = catalog(operation)
        .into_iter()
        .find(|option| option.format == format)?;
    // Preserve the current auxiliary/chapter contract until explicit track
    // composition supplies the actual retained/replaced output inventory.
    if !info.chapters.is_empty() && format != OutputFormat::Matroska {
        return None;
    }
    if info
        .streams
        .iter()
        .filter(|stream| {
            !matches!(stream.kind, StreamKind::Video | StreamKind::Audio)
                || stream
                    .video
                    .as_ref()
                    .is_some_and(|video| video.attached_picture)
        })
        .any(|stream| !copy_compatible(format, stream))
    {
        return None;
    }

    let video: Vec<_> = info
        .streams
        .iter()
        .filter(|stream| primary_video(stream))
        .collect();
    let audio: Vec<_> = info
        .streams
        .iter()
        .filter(|stream| stream.kind == StreamKind::Audio)
        .collect();
    if operation == Operation::VideoConverter && video.is_empty()
        || operation == Operation::AudioConverter && audio.is_empty()
    {
        return None;
    }
    let copies_video = video.iter().all(|stream| copy_compatible(format, stream));
    let copies_audio = audio.iter().all(|stream| copy_compatible(format, stream));
    option.video_codecs.retain(|codec| match codec {
        VideoCodec::Copy => copies_video,
        VideoCodec::Automatic => video.iter().all(|stream| {
            copy_compatible(format, stream)
                || stream.decoder_available
                    && stream.video.as_ref().is_some_and(|source| {
                        video_encoding_settings(format, *codec, source, 23)
                            .is_ok_and(|settings| encoder_accepted(format, &settings.encoder))
                    })
        }),
        _ => video.iter().all(|stream| {
            stream.decoder_available
                && stream.video.as_ref().is_some_and(|source| {
                    crate::processing::encoding::video_settings(*codec, source, 23).is_ok()
                })
        }),
    });
    let audio_encoder_available = option
        .audio_codecs
        .iter()
        .any(|codec| !matches!(codec, AudioCodec::Automatic | AudioCodec::Copy));
    option.audio_codecs.retain(|codec| match codec {
        AudioCodec::Copy => copies_audio,
        AudioCodec::Automatic => {
            audio
                .iter()
                .all(|stream| copy_compatible(format, stream) || stream.decoder_available)
                && (copies_audio || audio_encoder_available)
        }
        _ => audio.iter().all(|stream| {
            stream.decoder_available
                || copy_compatible(format, stream)
                    && (codec.codec_name() == Some(stream.codec.as_str())
                        || *codec == AudioCodec::Pcm && stream.codec.starts_with("pcm_"))
                    && (*codec != AudioCodec::Aac || stream.profile.as_deref() == Some("LC"))
        }),
    });
    if operation == Operation::VideoConverter && option.video_codecs.is_empty()
        || !audio.is_empty() && option.audio_codecs.is_empty()
    {
        return None;
    }
    option.format = requested;
    if requested == OutputFormat::Original {
        option.label = "Original".into();
    }
    Some(option)
}

pub(crate) fn source_catalog(info: &MediaInfo, operation: Operation) -> Vec<FormatOption> {
    product_formats(operation)
        .into_iter()
        .filter_map(|format| source_option(info, operation, format))
        .collect()
}

#[derive(Clone, Copy)]
enum AudioChoice {
    Rate,
    Channels,
    Layout,
    Precision,
    Bitrate,
}

fn lossless_codec(codec: AudioCodec) -> bool {
    matches!(
        codec,
        AudioCodec::PcmS16le
            | AudioCodec::Pcm
            | AudioCodec::Flac
            | AudioCodec::Alac
            | AudioCodec::Wavpack
    )
}

fn push_unique<T: PartialEq>(values: &mut Vec<T>, value: T) {
    if !values.contains(&value) {
        values.push(value);
    }
}

fn audio_choice_supported(
    stream: &crate::StreamInfo,
    format: OutputFormat,
    config: &ConversionConfig,
    choice: AudioChoice,
) -> bool {
    let Some(source) = &stream.audio else {
        return false;
    };
    let mut projected = config.clone();
    if !matches!(choice, AudioChoice::Bitrate) {
        if !matches!(choice, AudioChoice::Rate) {
            projected.sample_rate = None;
        }
        if !matches!(choice, AudioChoice::Channels) {
            projected.channels = None;
        }
        if !matches!(choice, AudioChoice::Layout) {
            projected.channel_layout = None;
        }
        if !matches!(choice, AudioChoice::Precision) {
            projected.audio_precision = None;
        }
        projected.audio_bitrate = None;
    }
    let compatible = copy_compatible(format, stream);
    if projected.audio_codec == AudioCodec::Copy {
        return compatible && !audio_transform_requested(stream, &projected);
    }
    if !audio_encoding_required(stream, &projected, compatible)
        && !audio_encoding_required(stream, config, compatible)
    {
        return true;
    }
    if !stream.decoder_available {
        return false;
    }
    let Ok(codec) = resolve_audio_codec(format, projected.audio_codec, stream) else {
        return false;
    };

    // Project one field's applicability without silently editing retained user
    // choices in other fields. Only bitrate depends on the current full tuple.
    let mut rates = Vec::new();
    if matches!(choice, AudioChoice::Rate | AudioChoice::Bitrate) {
        let Some(rate) = projected.sample_rate.or(source.sample_rate) else {
            return false;
        };
        rates.push(rate);
    } else {
        if let Some(rate) = config.sample_rate.or(source.sample_rate) {
            push_unique(&mut rates, rate);
        }
        for rate in [48000, 44100, 8000, 16000]
            .into_iter()
            .chain(SAMPLE_RATES.into_iter().filter(|rate| *rate != 0))
        {
            push_unique(&mut rates, rate);
        }
    }
    let mut layouts = Vec::new();
    if matches!(
        choice,
        AudioChoice::Channels | AudioChoice::Layout | AudioChoice::Bitrate
    ) {
        let Ok(layout) = requested_audio_layout(source, &projected) else {
            return false;
        };
        layouts.push(layout);
    } else {
        if let Ok(layout) = requested_audio_layout(source, config) {
            push_unique(&mut layouts, layout);
        }
        for layout in AUDIO_LAYOUTS
            .into_iter()
            .filter(|layout| *layout != AudioLayout::Original)
        {
            push_unique(
                &mut layouts,
                (
                    layout.channels().unwrap(),
                    layout.name().unwrap().to_owned(),
                ),
            );
        }
    }
    let precisions = if matches!(choice, AudioChoice::Precision | AudioChoice::Bitrate) {
        vec![projected
            .audio_precision
            .unwrap_or(AudioPrecision::Original)]
    } else if lossless_codec(codec) {
        let mut values = vec![config.audio_precision.unwrap_or(AudioPrecision::Original)];
        for value in AUDIO_PRECISIONS {
            push_unique(&mut values, value);
        }
        values
    } else {
        vec![AudioPrecision::Original]
    };
    for rate in rates {
        for (channels, layout) in &layouts {
            for precision in &precisions {
                if crate::processing::encoding::audio_settings(
                    codec,
                    format,
                    source,
                    rate,
                    *channels,
                    layout,
                    *precision,
                    projected.audio_bitrate,
                )
                .is_ok_and(|settings| encoder_accepted(format, &settings.encoder))
                {
                    return true;
                }
            }
        }
    }
    false
}

pub(crate) fn choices(info: &MediaInfo, config: &ConversionConfig) -> ConfigurationChoices {
    use crate::StreamKind;
    let video_facts = info
        .streams
        .iter()
        .find(|stream| primary_video(stream))
        .and_then(|stream| stream.video.as_ref());
    let (default_custom_width, default_custom_height, default_custom_frame_rate) =
        custom_geometry_defaults(video_facts);
    let mut result = ConfigurationChoices {
        video_sizes: Vec::new(),
        frame_rates: vec![FrameRate::Original],
        sample_rates: Vec::new(),
        channels: Vec::new(),
        audio_bitrates: Vec::new(),
        channel_layouts: Vec::new(),
        audio_precisions: Vec::new(),
        default_custom_width,
        default_custom_height,
        default_custom_frame_rate,
    };
    let Ok(format) = resolve_format(info, config) else {
        return result;
    };
    let Some(option) = source_option(info, config.operation, config.format) else {
        return result;
    };
    let video: Vec<_> = info
        .streams
        .iter()
        .filter(|stream| primary_video(stream))
        .collect();
    if video.is_empty() {
        result.video_sizes.push(VideoSize::Original);
    } else {
        let can_encode = option.video_codecs.contains(&config.video_codec)
            && config.video_codec != VideoCodec::Copy
            && video.iter().all(|stream| stream.decoder_available);
        let encode_valid = |candidate: &ConversionConfig| {
            can_encode
                && video.iter().all(|stream| {
                    stream.video.as_ref().is_some_and(|source| {
                        video_encoding_settings(
                            format,
                            candidate.video_codec,
                            source,
                            candidate.quality,
                        )
                        .is_ok_and(|settings| {
                            settings.pixel_format.as_deref().is_some_and(|pixel| {
                                configured_encoding_dimensions(source, candidate, pixel).is_ok()
                            }) && encoder_accepted(format, &settings.encoder)
                        })
                    })
                })
        };
        let can_copy = option.video_codecs.contains(&VideoCodec::Copy)
            && matches!(config.video_codec, VideoCodec::Automatic | VideoCodec::Copy);
        for size in VIDEO_SIZES {
            let mut candidate = config.clone();
            candidate.video_size = size;
            if size == VideoSize::Custom {
                candidate.custom_width = Some(config.custom_width.unwrap_or(default_custom_width));
                candidate.custom_height =
                    Some(config.custom_height.unwrap_or(default_custom_height));
            }
            let unchanged = video.iter().all(|stream| {
                stream.video.as_ref().is_some_and(|source| {
                    configured_video_dimensions(source, &candidate)
                        .ok()
                        .zip(scaled_video_dimensions(source, VideoSize::Original).ok())
                        .is_some_and(|(selected, original)| selected == original)
                })
            });
            if can_copy
                && unchanged
                && config.frame_rate == FrameRate::Original
                && config.quality == 23
                || encode_valid(&candidate)
            {
                result.video_sizes.push(size);
            }
        }
        let mut geometry = config.clone();
        if geometry.video_size == VideoSize::Custom {
            geometry.custom_width.get_or_insert(default_custom_width);
            geometry.custom_height.get_or_insert(default_custom_height);
        }
        if encode_valid(&geometry) {
            result.frame_rates = FRAME_RATES.to_vec();
        } else if !can_copy {
            result.frame_rates.clear();
        }
    }
    let audio: Vec<_> = info
        .streams
        .iter()
        .filter(|stream| stream.kind == StreamKind::Audio)
        .collect();
    if audio.is_empty() || !option.audio_codecs.contains(&config.audio_codec) {
        return result;
    }
    let supported = |candidate: &ConversionConfig, axis| {
        audio
            .iter()
            .all(|stream| audio_choice_supported(stream, format, candidate, axis))
    };
    for rate in SAMPLE_RATES {
        let mut candidate = config.clone();
        candidate.sample_rate = (rate != 0).then_some(rate);
        if supported(&candidate, AudioChoice::Rate) {
            result.sample_rates.push(rate);
        }
    }
    for channels in [0, 1, 2, 6, 8] {
        let mut candidate = config.clone();
        candidate.channels = (channels != 0).then_some(channels);
        candidate.channel_layout = None;
        if supported(&candidate, AudioChoice::Channels) {
            result.channels.push(channels);
        }
    }
    for layout in AUDIO_LAYOUTS {
        let mut candidate = config.clone();
        candidate.channel_layout = Some(layout);
        candidate.channels = None;
        if supported(&candidate, AudioChoice::Layout) {
            result.channel_layouts.push(layout);
        }
    }
    let codecs: Option<Vec<_>> = audio
        .iter()
        .map(|stream| resolve_audio_codec(format, config.audio_codec, stream).ok())
        .collect();
    if config.audio_codec != AudioCodec::Copy
        && codecs
            .as_ref()
            .is_some_and(|codecs| codecs.iter().all(|codec| lossless_codec(*codec)))
    {
        for precision in AUDIO_PRECISIONS {
            let mut candidate = config.clone();
            candidate.audio_precision = Some(precision);
            if supported(&candidate, AudioChoice::Precision) {
                result.audio_precisions.push(precision);
            }
        }
    }
    if config.audio_codec != AudioCodec::Copy
        && codecs
            .as_ref()
            .is_some_and(|codecs| codecs.iter().all(|codec| !lossless_codec(*codec)))
    {
        for bitrate in [
            0, 4750, 5150, 5900, 6000, 6600, 6700, 7400, 7950, 8000, 8850, 10200, 12200, 12650,
            14250, 15850, 16000, 18250, 19850, 23050, 23850, 24000, 32000, 40000, 48000, 56000,
            64000, 80000, 96000, 112000, 128000, 160000, 192000, 224000, 256000, 320000, 384000,
            448000, 512000, 576000, 640000, 768000, 1024000, 1536000,
        ] {
            let mut candidate = config.clone();
            candidate.audio_bitrate = (bitrate != 0).then_some(bitrate);
            if supported(&candidate, AudioChoice::Bitrate) {
                result.audio_bitrates.push(bitrate);
            }
        }
    }
    result
}
