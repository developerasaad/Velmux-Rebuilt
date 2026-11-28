mod adjustments;
mod compression;
pub(crate) mod encoding;
mod execution;
mod transcode;
pub(crate) use execution::execute_conversion;

use crate::{configuration::*, ffmpeg::sys, EngineError, MediaInfo, StreamKind};
use serde::{Deserialize, Serialize};
use std::ffi::CString;

pub(crate) use transcode::convert;
pub(crate) enum ProcessingProgress {
    Started,
    Timestamp(i64),
    Finalizing,
    Restarted,
    Validating,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct StreamPlan {
    pub index: u32,
    pub encoder: Option<String>,
    pub expected_codec: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub sample_rate: Option<u32>,
    pub channels: Option<u32>,
    pub bit_depth: Option<u32>,
    #[serde(default)]
    pub encoding: Option<encoding::EncodingSettings>,
    #[serde(default)]
    pub frame_rate: Option<crate::Rational>,
    #[serde(default)]
    pub audio_filters: Option<adjustments::AudioFilterPlan>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Plan {
    pub config: ConversionConfig,
    pub streams: Vec<StreamPlan>,
    #[serde(default)]
    pub output: Option<OutputRepresentation>,
    #[serde(default)]
    pub preview_range: Option<PreviewRange>,
    #[serde(default)]
    pub target_size_bytes: Option<u64>,
}

impl Plan {
    pub(crate) fn output(&self) -> Result<OutputRepresentation, EngineError> {
        // M2 accepted plans always stored an explicit format. Retaining that
        // exact representation does not invoke newer configuration defaults.
        let output = self.output.clone().unwrap_or_else(|| {
            let mut legacy = OutputRepresentation::from_format(self.config.format);
            if self.config.format == OutputFormat::M4a {
                legacy.muxer = "ipod".into();
            }
            legacy
        });
        output.validate()?;
        Ok(output)
    }
}

/// Logical elapsed source time; conversion adds the common inspected source
/// origin before seeking. This never changes a submitted user configuration.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct PreviewRange {
    pub start_us: i64,
    pub duration_us: i64,
}

pub(crate) fn common_origin(info: &MediaInfo) -> Option<i64> {
    info.start_time_us.or_else(|| {
        info.streams
            .iter()
            .filter_map(|stream| stream.start_time_us)
            .min()
    })
}

pub(crate) fn preview_plan(info: &MediaInfo, start_us: i64) -> Result<Plan, EngineError> {
    if start_us < 0
        || info
            .duration_us
            .is_some_and(|duration| start_us >= duration)
    {
        return Err(invalid("The preview position lies outside this source."));
    }
    let duration_us = info
        .duration_us
        .map_or(15_000_000, |duration| (duration - start_us).min(15_000_000));
    if duration_us <= 0 || start_us.checked_add(duration_us).is_none() {
        return Err(invalid("The preview time range is invalid."));
    }
    let origin = common_origin(info).unwrap_or(0);
    origin
        .checked_add(start_us)
        .and_then(|start| start.checked_add(duration_us))
        .ok_or_else(|| invalid("The preview timestamps exceed the supported range."))?;
    let video = info.streams.iter().find(|stream| {
        stream.kind == StreamKind::Video
            && !stream
                .video
                .as_ref()
                .is_some_and(|video| video.attached_picture)
    });
    let audio = info
        .streams
        .iter()
        .filter(|stream| stream.kind == StreamKind::Audio)
        .filter(|stream| {
            let Some(start) = stream
                .start_time_us
                .and_then(|start| start.checked_sub(origin))
            else {
                return true;
            };
            if start >= start_us + duration_us {
                return false;
            }
            stream
                .duration_us
                .and_then(|duration| start.checked_add(duration))
                .is_none_or(|end| end > start_us)
        })
        .min_by_key(|stream| {
            (
                u8::from(stream.disposition & sys::AV_DISPOSITION_DEFAULT == 0),
                stream.index,
            )
        });
    let mut streams = Vec::new();
    if let Some(stream) = video {
        if !stream.decoder_available {
            return Err(invalid(
                "A decoder is required to prepare this video preview.",
            ));
        }
        let facts = stream
            .video
            .as_ref()
            .ok_or_else(|| invalid("The preview geometry is unknown."))?;
        let width = facts
            .width
            .ok_or_else(|| invalid("The preview width is unknown."))?;
        let height = facts
            .height
            .ok_or_else(|| invalid("The preview height is unknown."))?;
        if width == 0 || height == 0 || u64::from(width) * u64::from(height) > 64 * 1024 * 1024 {
            return Err(invalid(
                "This source exceeds the preview decoder's geometry limit.",
            ));
        }
        let sar = facts.sample_aspect_ratio.as_ref().map_or(1.0, |ratio| {
            f64::from(ratio.numerator) / f64::from(ratio.denominator)
        });
        if !sar.is_finite() || sar <= 0.0 {
            return Err(invalid("The preview pixel geometry is invalid."));
        }
        let factor = (640.0 / (f64::from(width) * sar).max(f64::from(height))).min(1.0);
        let out_width = ((f64::from(width) * factor).floor() as u32 / 2) * 2;
        let out_height = ((f64::from(height) * factor).floor() as u32 / 2) * 2;
        if out_width < 2 || out_height < 2 {
            return Err(invalid(
                "The source aspect ratio cannot produce a usable preview.",
            ));
        }
        let known_hdr = facts
            .color_transfer
            .as_deref()
            .is_some_and(|transfer| matches!(transfer, "smpte2084" | "arib-std-b67"));
        if (facts.mastering_display.is_some() || facts.content_light.is_some()) && !known_hdr {
            return Err(invalid(
                "This HDR source has no usable transfer information for a faithful preview.",
            ));
        }
        if known_hdr && (facts.color_primaries.is_none() || facts.color_space.is_none()) {
            return Err(invalid(
                "This HDR source has incomplete color information for preview tone mapping.",
            ));
        }
        streams.push(StreamPlan {
            index: stream.index,
            encoder: Some("libx264".into()),
            expected_codec: "h264".into(),
            width: Some(out_width),
            height: Some(out_height),
            sample_rate: None,
            channels: None,
            bit_depth: None,
            encoding: None,
            frame_rate: None,
            audio_filters: None,
        });
    }
    if let Some(stream) = audio {
        if !stream.decoder_available {
            return Err(invalid(
                "A decoder is required to prepare this audio preview.",
            ));
        }
        let facts = stream
            .audio
            .as_ref()
            .ok_or_else(|| invalid("The preview audio properties are unknown."))?;
        let channels = facts
            .channels
            .ok_or_else(|| invalid("The preview channel layout is unknown."))?
            .min(2);
        if channels == 0 {
            return Err(invalid("The preview channel layout is invalid."));
        }
        let rate = if facts.sample_rate == Some(44_100) {
            44_100
        } else {
            48_000
        };
        streams.push(StreamPlan {
            index: stream.index,
            encoder: Some(if video.is_some() { "aac" } else { "pcm_s16le" }.into()),
            expected_codec: if video.is_some() { "aac" } else { "pcm_s16le" }.into(),
            width: None,
            height: None,
            sample_rate: Some(rate),
            channels: Some(channels),
            bit_depth: None,
            encoding: None,
            frame_rate: None,
            audio_filters: None,
        });
    }
    if streams.is_empty() {
        return Err(invalid(
            "No playable media stream is available for preview.",
        ));
    }
    let config = ConversionConfig {
        operation: if video.is_some() {
            Operation::VideoConverter
        } else {
            Operation::AudioConverter
        },
        format: if video.is_some() {
            OutputFormat::Mp4
        } else {
            OutputFormat::Wav
        },
        video_codec: if video.is_some() {
            VideoCodec::H264
        } else {
            VideoCodec::Automatic
        },
        audio_codec: if video.is_some() {
            AudioCodec::Aac
        } else {
            AudioCodec::PcmS16le
        },
        video_size: VideoSize::Original,
        frame_rate: if video.is_some() {
            FrameRate::Fps30
        } else {
            FrameRate::Original
        },
        quality: 23,
        audio_bitrate: Some(if video.is_some() { 128_000 } else { 192_000 }),
        sample_rate: None,
        channels: None,
        file_name: "preview".into(),
        custom_width: None,
        custom_height: None,
        custom_frame_rate: None,
        audio_precision: None,
        channel_layout: None,
        compression: None,
        audio_adjustments: None,
    };
    Ok(Plan {
        output: Some(OutputRepresentation::from_format(config.format)),
        config,
        streams,
        target_size_bytes: None,
        preview_range: Some(PreviewRange {
            start_us,
            duration_us,
        }),
    })
}

fn codec_id(name: &str) -> Result<i32, EngineError> {
    let name = CString::new(name).map_err(|_| invalid("Invalid codec name"))?;
    unsafe {
        let codec = sys::avcodec_find_encoder_by_name(name.as_ptr());
        if codec.is_null() {
            return Err(invalid(
                "The required encoder is unavailable in this build.",
            ));
        }
        Ok((*codec).id as i32)
    }
}

pub(crate) fn validate_operation(
    info: &MediaInfo,
    operation: Operation,
) -> Result<(), EngineError> {
    let video = info
        .streams
        .iter()
        .filter(|s| {
            s.kind == StreamKind::Video && !s.video.as_ref().is_some_and(|v| v.attached_picture)
        })
        .count();
    let audio = info
        .streams
        .iter()
        .filter(|s| s.kind == StreamKind::Audio)
        .count();
    if operation == Operation::VideoConverter && video != 1 {
        return Err(invalid(
            "Video Converter currently requires exactly one primary video stream.",
        ));
    }
    if operation == Operation::AudioConverter && (audio != 1 || video != 0) {
        return Err(invalid("Audio Converter currently accepts one audio stream in an audio source. Video extraction and additional track selection are not available yet."));
    }
    Ok(())
}

pub(crate) fn plan(info: &MediaInfo, config: &ConversionConfig) -> Result<Plan, EngineError> {
    validate_fields(config)?;
    validate_operation(info, config.operation)?;
    let audio_filters = adjustments::audio_plan(info, config.audio_adjustments.as_ref())?;
    let accepted = config;
    let effective = effective_config(info, config)?;
    let config = &effective;
    let format = resolve_format(info, config)?;
    let mut resolved = accepted.clone();
    resolved.format = format;
    // Original remains immutable user intent; compatibility is evaluated against
    // its actual resolved representation before a job can be accepted.
    validate_fields(&resolved)?;
    if !info.chapters.is_empty() && format != OutputFormat::Matroska {
        return Err(invalid("Choose Matroska to preserve the source chapters."));
    }
    if !info
        .streams
        .iter()
        .any(|stream| stream.kind == StreamKind::Audio)
        && (!matches!(config.audio_codec, AudioCodec::Automatic | AudioCodec::Copy)
            || config.audio_bitrate.is_some()
            || config.sample_rate.is_some()
            || config.channels.is_some()
            || config
                .channel_layout
                .is_some_and(|layout| layout != AudioLayout::Original)
            || config
                .audio_precision
                .is_some_and(|precision| precision != AudioPrecision::Original)
            || config.compression.as_ref().is_some_and(|settings| {
                settings.reduce_file_size && settings.custom_audio.is_some()
            }))
    {
        return Err(invalid(
            "This source has no audio stream to apply these settings to.",
        ));
    }
    let muxer_name = CString::new(format.muxer()).unwrap();
    let muxer =
        unsafe { sys::av_guess_format(muxer_name.as_ptr(), std::ptr::null(), std::ptr::null()) };
    if muxer.is_null() {
        return Err(invalid(
            "The selected output format is unavailable in this build.",
        ));
    }
    if format == OutputFormat::Flv
        && info
            .streams
            .iter()
            .filter(|stream| stream.kind == StreamKind::Audio)
            .count()
            > 1
    {
        return Err(invalid("FLV supports one audio track. Preserve all tracks in another container or explicitly choose the intended tracks."));
    }
    let mut streams = Vec::new();
    for stream in &info.streams {
        let mut item = StreamPlan {
            index: stream.index,
            encoder: None,
            expected_codec: stream.codec.clone(),
            width: None,
            height: None,
            sample_rate: None,
            channels: None,
            bit_depth: None,
            encoding: None,
            frame_rate: None,
            audio_filters: None,
        };
        if stream.kind == StreamKind::Video
            && !stream
                .video
                .as_ref()
                .is_some_and(|video| video.attached_picture)
        {
            if let Some(output) = resolve_video_output(stream, format, config)? {
                item.encoder = Some(output.settings.encoder.clone());
                item.expected_codec = output.settings.codec.clone();
                item.width = Some(output.width);
                item.height = Some(output.height);
                item.bit_depth = output.settings.bit_depth;
                item.frame_rate = output.frame_rate;
                item.encoding = Some(output.settings);
            }
        } else if stream.kind == StreamKind::Audio {
            item.audio_filters = audio_filters.clone();
            if let Some(AudioOutput {
                sample_rate,
                channels,
                settings,
            }) = resolve_audio_output(stream, format, config)?
            {
                item.encoder = Some(settings.encoder.clone());
                item.expected_codec = settings.codec.clone();
                item.sample_rate = Some(sample_rate);
                item.channels = Some(channels);
                item.bit_depth = settings.bit_depth;
                item.encoding = Some(settings);
            }
        }
        let target_id = if let Some(encoder) = &item.encoder {
            codec_id(encoder)?
        } else {
            stream.codec_id
        };
        if !muxer_accepts(format, target_id)
            || stream.kind == StreamKind::Audio && !audio_copy_allowed(format, &item.expected_codec)
        {
            return Err(invalid(format!("{} stream {} is incompatible with this container. Choose a compatible representation or an explicit track decision; tracks are never silently discarded.",stream.codec,stream.index)));
        }
        streams.push(item);
    }
    let mut plan = Plan {
        output: Some(OutputRepresentation::from_format(format)),
        config: accepted.clone(),
        streams,
        preview_range: None,
        target_size_bytes: None,
    };
    compression::apply_to_plan(info, &mut plan)?;
    Ok(plan)
}

pub(crate) fn verify_output(
    info: &MediaInfo,
    source: &MediaInfo,
    plan: &Plan,
) -> Result<(), EngineError> {
    if info.streams.len() != plan.streams.len() {
        return Err(EngineError::InvalidMedia {
            detail: "The finalized output has an unexpected stream count.".into(),
        });
    }
    for (stream, expected) in info.streams.iter().zip(&plan.streams) {
        if stream.codec != expected.expected_codec {
            return Err(EngineError::InvalidMedia {
                detail: format!("Output stream {} has an unexpected codec.", stream.index),
            });
        }
        if let Some(width) = expected.width {
            let video = stream
                .video
                .as_ref()
                .ok_or_else(|| invalid("The output video stream is missing."))?;
            if video.width != Some(width) || video.height != expected.height {
                return Err(EngineError::InvalidMedia {
                    detail:
                        "The finalized video geometry does not match the accepted configuration."
                            .into(),
                });
            }
        }
        if expected.width.is_some() && plan.preview_range.is_none() {
            if let Some(requested) = expected.frame_rate.clone().or(plan
                .config
                .frame_rate
                .rational(plan.config.custom_frame_rate.as_ref())?)
            {
                let actual = stream
                    .video
                    .as_ref()
                    .and_then(|video| video.average_frame_rate.as_ref())
                    .ok_or_else(|| {
                        invalid("The finalized video has no verifiable requested frame rate.")
                    })?;
                let rate = f64::from(actual.numerator) / f64::from(actual.denominator);
                let required = f64::from(requested.numerator) / f64::from(requested.denominator);
                if !rate.is_finite() || (rate - required).abs() > 0.01 {
                    return Err(EngineError::InvalidMedia {
                        detail:
                            "The finalized frame rate does not match the accepted configuration."
                                .into(),
                    });
                }
            }
        }
        if let Some(settings) = &expected.encoding {
            if expected.width.is_some() {
                let video = stream
                    .video
                    .as_ref()
                    .ok_or_else(|| invalid("The encoded video stream is missing."))?;
                if settings
                    .bit_depth
                    .is_some_and(|depth| video.bit_depth != Some(depth))
                    || settings
                        .color_space
                        .as_ref()
                        .is_some_and(|value| video.color_space.as_ref() != Some(value))
                    || settings
                        .color_range
                        .as_ref()
                        .is_some_and(|value| video.color_range.as_ref() != Some(value))
                {
                    return Err(EngineError::InvalidMedia { detail:"The finalized video precision or color representation differs from the accepted requirements.".into() });
                }
                let original = source
                    .streams
                    .iter()
                    .find(|item| item.index == expected.index)
                    .and_then(|item| item.video.as_ref())
                    .ok_or_else(|| invalid("The accepted video source is missing."))?;
                encoding::verify_video_representation(video, original, settings)?;
                if original.color_primaries.is_some()
                    && video.color_primaries != original.color_primaries
                    || original.color_transfer.is_some()
                        && video.color_transfer != original.color_transfer
                {
                    return Err(EngineError::InvalidMedia { detail:"The finalized output did not preserve the accepted video color characteristics.".into() });
                }
            }
            if let Some(layout) = &settings.channel_layout {
                let actual = stream
                    .audio
                    .as_ref()
                    .and_then(|audio| audio.channel_layout.as_deref())
                    .ok_or_else(|| {
                        invalid("The finalized speaker layout could not be verified.")
                    })?;
                if actual != layout {
                    return Err(EngineError::InvalidMedia { detail:format!("The finalized audio speaker layout ({actual}) differs from the accepted requirements ({layout}).") });
                }
            }
        }
        if let Some(rate) = expected.sample_rate {
            let audio = stream
                .audio
                .as_ref()
                .ok_or_else(|| invalid("The output audio stream is missing."))?;
            if expected.bit_depth.is_some() && audio.bit_depth != expected.bit_depth {
                return Err(EngineError::InvalidMedia {
                    detail:
                        "The finalized audio precision does not match the accepted configuration."
                            .into(),
                });
            }
            if audio.sample_rate != Some(rate) || audio.channels != expected.channels {
                return Err(EngineError::InvalidMedia {
                    detail:
                        "The finalized audio sampling does not match the accepted configuration."
                            .into(),
                });
            }
        }
    }
    if let Some(range) = &plan.preview_range {
        let actual = info
            .duration_us
            .ok_or_else(|| invalid("The preview has no verifiable duration."))?;
        if actual <= 0 || actual > range.duration_us.saturating_add(250_000) {
            return Err(invalid("The preview exceeds its bounded time range."));
        }
        return Ok(());
    }
    if let Some(expected) = source.duration_us.filter(|value| *value > 0) {
        let actual = info.duration_us.ok_or_else(|| EngineError::InvalidMedia {
            detail: "The finalized output has no verifiable duration.".into(),
        })?;
        if actual <= 0 || actual.abs_diff(expected) > 500_000u64.max(expected as u64 / 100) {
            return Err(EngineError::InvalidMedia {
                detail: "The finalized duration differs unexpectedly from the source.".into(),
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod preview_tests {
    use super::*;
    use crate::{AudioInfo, StreamInfo};

    fn audio_source() -> MediaInfo {
        MediaInfo {
            source_identity: "fixture-samples".into(),
            byte_size: Some(960_000),
            container: "wav".into(),
            container_long_name: None,
            duration_us: Some(60_000_000),
            start_time_us: Some(10_000_000),
            bit_rate: Some(128_000),
            metadata: Vec::new(),
            chapters: Vec::new(),
            staged: false,
            streams: vec![StreamInfo {
                id: "fixture:0".into(),
                index: 0,
                container_id: 0,
                kind: StreamKind::Audio,
                codec_id: 0,
                codec: "pcm_s16le".into(),
                codec_long_name: None,
                decoder_available: true,
                time_base: Some(crate::Rational {
                    numerator: 1,
                    denominator: 8000,
                }),
                duration_us: Some(60_000_000),
                start_time_us: Some(10_000_000),
                frame_count: None,
                bit_rate: Some(128_000),
                disposition: 0,
                profile: None,
                level: None,
                metadata: Vec::new(),
                video: None,
                subtitle: None,
                audio: Some(AudioInfo {
                    sample_rate: Some(8000),
                    channels: Some(1),
                    channel_layout: Some("mono".into()),
                    sample_format: Some("s16".into()),
                    bit_depth: Some(16),
                    initial_padding: 0,
                    trailing_padding: 0,
                    seek_preroll: 0,
                }),
            }],
        }
    }

    #[test]
    fn preview_ranges_are_logical_bounded_and_do_not_change_source_facts() {
        let source = audio_source();
        let plan = preview_plan(&source, 25_000_000).unwrap();
        let range = plan.preview_range.unwrap();
        assert_eq!(range.start_us, 25_000_000);
        assert_eq!(range.duration_us, 15_000_000);
        assert_eq!(plan.config.format, OutputFormat::Wav);
        assert_eq!(plan.streams[0].expected_codec, "pcm_s16le");
        assert_eq!(common_origin(&source), Some(10_000_000));
        assert_eq!(source.duration_us, Some(60_000_000));
        assert_eq!(
            preview_plan(&source, 59_000_000)
                .unwrap()
                .preview_range
                .unwrap()
                .duration_us,
            1_000_000
        );
        assert!(preview_plan(&source, -1).is_err());
        assert!(preview_plan(&source, 60_000_000).is_err());
    }

    #[test]
    fn preview_rejects_overflow_and_tracks_outside_the_requested_range() {
        let mut source = audio_source();
        source.duration_us = None;
        assert!(preview_plan(&source, i64::MAX).is_err());
        source.streams[0].start_time_us = Some(30_000_000);
        assert!(preview_plan(&source, 0).is_err());
    }
}
