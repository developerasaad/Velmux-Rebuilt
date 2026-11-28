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
    AnalysisTimestamp(i64),
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
    #[serde(default)]
    pub video_transform: Option<VideoTransformPlan>,
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
            video_transform: None,
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
            video_transform: None,
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
        video_adjustments: None,
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
    if config
        .video_adjustments
        .as_ref()
        .is_some_and(|settings| settings.active())
        && info.streams.iter().any(|stream| {
            stream.subtitle.as_ref().is_some_and(|subtitle| {
                subtitle.representation != crate::SubtitleRepresentation::Text
            })
        })
    {
        return Err(invalid("Bitmap or unknown subtitle placement cannot yet be preserved through spatial video adjustments. Keep these adjustments off or choose a source without those tracks; no subtitle is silently discarded."));
    }
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
            video_transform: None,
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
                item.video_transform = output.transform;
                item.encoding = Some(output.settings);
            }
        } else if stream.kind == StreamKind::Audio {
            item.audio_filters =
                adjustments::audio_plan(info, stream, config.audio_adjustments.as_ref())?;
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

fn audio_output_tolerance(
    actual: &crate::StreamInfo,
    expected: &StreamPlan,
    filters: &adjustments::AudioFilterPlan,
    source: &MediaInfo,
) -> Result<u64, EngineError> {
    let rate = expected.sample_rate.unwrap_or(1).max(1);
    let codec_samples = match expected.expected_codec.as_str() {
        codec if codec.starts_with("pcm_") || matches!(codec, "flac" | "alac" | "wavpack") => 2,
        "aac" => 2048,
        "mp3" => 2304,
        "ac3" | "eac3" => 3072,
        "opus" => 5760,
        _ => 8192,
    };
    let sample_time = (codec_samples * 1_000_000u64).div_ceil(u64::from(rate));
    let tick = actual
        .time_base
        .as_ref()
        .filter(|base| base.numerator > 0 && base.denominator > 0)
        .map_or(0, |base| {
            (base.numerator as u64 * 1_000_000).div_ceil(base.denominator as u64)
        });
    let original = source
        .streams
        .iter()
        .find(|stream| stream.index == expected.index)
        .ok_or_else(|| invalid("The accepted audio source stream is missing."))?;
    filters
        .timing_tolerance_us(original)?
        .unwrap_or(0)
        .checked_add(sample_time)
        .and_then(|value| value.checked_add(tick))
        .ok_or_else(|| invalid("The output audio timing tolerance exceeds supported bounds."))
}

pub(crate) fn verify_output(
    info: &MediaInfo,
    source: &MediaInfo,
    plan: &Plan,
) -> Result<(), EngineError> {
    verify_output_with_evidence(info, source, plan, DurationEvidence::Fresh)
}

/// Exact previously verified bytes retain their original measured-duration
/// proof. The capability is constructed privately by Jobs only after checking
/// its committed verified flag and complete size/hash, never by a fresh encode.
pub(crate) fn verify_retained_output(
    retained: &crate::jobs::VerifiedWorkingOutput<'_>,
    source: &MediaInfo,
    plan: &Plan,
    token: std::sync::Arc<crate::CancellationToken>,
) -> Result<(), EngineError> {
    let info = crate::media::inspect_file(retained.path(), token)?;
    verify_output_with_evidence(&info, source, plan, DurationEvidence::RetainedVerifiedBytes)
}

#[derive(Clone, Copy)]
enum DurationEvidence {
    Fresh,
    RetainedVerifiedBytes,
}
impl DurationEvidence {
    fn reuses_measurement(self, filters: &adjustments::AudioFilterPlan) -> bool {
        matches!(self, Self::RetainedVerifiedBytes) && filters.needs_measurement()
    }
}

fn verify_output_with_evidence(
    info: &MediaInfo,
    source: &MediaInfo,
    plan: &Plan,
    duration_evidence: DurationEvidence,
) -> Result<(), EngineError> {
    if matches!(duration_evidence, DurationEvidence::Fresh)
        && plan.streams.iter().any(|stream| {
            stream
                .audio_filters
                .as_ref()
                .is_some_and(|filters| filters.needs_measurement())
        })
    {
        return Err(EngineError::Internal { detail: "Fresh output verification requires the accepted decoded-duration measurement before any verified commitment.".into() });
    }
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
        if let Some(spatial) = &expected.video_transform {
            let video = stream
                .video
                .as_ref()
                .ok_or_else(|| invalid("The transformed output video is missing."))?;
            let identity = [65536, 0, 0, 0, 65536, 0, 0, 0, 1 << 30];
            if !video.display_matrix.is_empty() && video.display_matrix != identity
                || video
                    .rotation_degrees_counter_clockwise
                    .is_some_and(|angle| !angle.is_finite() || angle.abs() > 0.00001)
                || info
                    .metadata
                    .iter()
                    .chain(stream.metadata.iter())
                    .any(|entry| {
                        entry.key.eq_ignore_ascii_case("rotate")
                            && entry
                                .value
                                .parse::<f64>()
                                .ok()
                                .is_none_or(|angle| !angle.is_finite() || angle.abs() > 0.00001)
                    })
            {
                return Err(EngineError::InvalidMedia {
                    detail: "The finalized video would apply display orientation a second time."
                        .into(),
                });
            }
            let actual = video
                .sample_aspect_ratio
                .clone()
                .unwrap_or(crate::Rational {
                    numerator: 1,
                    denominator: 1,
                });
            let required = &spatial.output_sample_aspect_ratio;
            if actual.numerator <= 0
                || actual.denominator <= 0
                || i128::from(actual.numerator) * i128::from(required.denominator)
                    != i128::from(required.numerator) * i128::from(actual.denominator)
            {
                return Err(EngineError::InvalidMedia {detail:"The finalized sample aspect ratio differs from the accepted spatial geometry.".into()});
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
            if let Some(filters) = &expected.audio_filters {
                if let Some(timing) = filters.timing() {
                    let tolerance = audio_output_tolerance(stream, expected, filters, source)?;
                    if !duration_evidence.reuses_measurement(filters)
                        && stream
                            .duration_us
                            .filter(|duration| *duration > 0)
                            .zip(timing.output_duration_us)
                            .is_some_and(|(actual, required)| {
                                actual.abs_diff(required as i64) > tolerance
                            })
                    {
                        return Err(EngineError::InvalidMedia { detail: format!("The finalized audio track {} duration does not match the accepted playback speed.", stream.index) });
                    }
                    let actual_start = stream
                        .start_time_us
                        .unwrap_or_else(|| common_origin(info).unwrap_or(0));
                    if actual_start.abs_diff(timing.output_start_us) > tolerance {
                        return Err(EngineError::InvalidMedia { detail: format!("The finalized audio track {} origin does not match the accepted playback speed.", stream.index) });
                    }
                }
            }
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
    if plan.streams.iter().any(|stream| {
        stream
            .audio_filters
            .as_ref()
            .is_some_and(|filters| duration_evidence.reuses_measurement(filters))
    }) {
        // The previous full validation used execution-local decoded timing.
        // Re-inspection above still checks composition, codec, geometry, color,
        // sampling and origins. Re-deriving duration from coded padding here
        // would reject the unchanged bytes whose measured proof was committed.
        if info.duration_us.is_none_or(|duration| duration <= 0) {
            return Err(invalid(
                "The retained verified output has no positive media duration.",
            ));
        }
        return Ok(());
    }
    let transformed = plan.streams.iter().any(|stream| {
        stream
            .audio_filters
            .as_ref()
            .and_then(|filters| filters.timing())
            .is_some()
    });
    if transformed {
        let mut end = 0i64;
        let mut start = i64::MAX;
        let mut tolerance = 0;
        for (actual, expected) in info.streams.iter().zip(&plan.streams) {
            let Some(filters) = &expected.audio_filters else {
                continue;
            };
            let Some(timing) = filters.timing() else {
                continue;
            };
            let duration = timing
                .output_duration_us
                .and_then(|value| i64::try_from(value).ok())
                .ok_or_else(|| invalid("The accepted transformed audio duration is missing."))?;
            let endpoint = timing
                .output_start_us
                .checked_add(duration)
                .ok_or_else(|| {
                    invalid("The transformed audio endpoint exceeds supported bounds.")
                })?;
            end = end.max(endpoint);
            start = start.min(timing.output_start_us);
            tolerance = tolerance.max(audio_output_tolerance(actual, expected, filters, source)?);
        }
        let required = end
            .checked_sub(start)
            .filter(|value| *value > 0)
            .ok_or_else(|| invalid("The transformed audio interval is invalid."))?;
        let actual = info.duration_us.filter(|value| *value > 0).ok_or_else(|| {
            invalid("The finalized transformed audio has no verifiable duration.")
        })?;
        if actual.abs_diff(required) > tolerance {
            return Err(EngineError::InvalidMedia {
                detail:
                    "The finalized duration differs from the accepted transformed audio timeline."
                        .into(),
            });
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
    fn bitmap_placement_cannot_be_silently_reused_after_spatial_transformation() {
        let source:MediaInfo=serde_json::from_value(serde_json::json!({"source_identity":"bitmap-canvas","container":"matroska","duration_us":1000000,"metadata":[],"chapters":[],"staged":false,"streams":[
            {"id":"v","index":0,"container_id":0,"kind":"Video","codec_id":sys::AV_CODEC_ID_H264,"codec":"h264","decoder_available":true,"metadata":[],"disposition":0,"video":{"width":160,"height":96,"pixel_format":"yuv420p","bit_depth":8,"attached_picture":false}},
            {"id":"s","index":1,"container_id":1,"kind":"Subtitle","codec_id":sys::AV_CODEC_ID_HDMV_PGS_SUBTITLE,"codec":"hdmv_pgs_subtitle","decoder_available":true,"metadata":[],"disposition":0,"subtitle":{"representation":"Bitmap"}}
        ]})).unwrap();
        let mut config = default_config(Operation::VideoConverter, &source, "bitmap.mkv");
        config.video_adjustments = Some(VideoAdjustments {
            horizontal_flip: true,
            ..Default::default()
        });
        assert!(
            matches!(plan(&source,&config),Err(EngineError::Configuration{detail}) if detail.contains("Bitmap"))
        );
        assert!(choices(&source, &config).video_adjustment_limits.is_none());
    }

    #[test]
    fn coarse_source_timestamp_precision_scales_with_speed_but_not_measured_timing() {
        let mut source = audio_source();
        source.container = "matroska".into();
        source.duration_us = Some(4_000_000);
        source.start_time_us = Some(0);
        source.streams[0].codec_id = sys::AV_CODEC_ID_PCM_S16LE as i32;
        source.streams[0].duration_us = None;
        source.streams[0].start_time_us = Some(0);
        source.streams[0].time_base = Some(crate::Rational {
            numerator: 1,
            denominator: 1000,
        });
        source.streams[0].audio.as_mut().unwrap().sample_rate = Some(48000);
        let mut config = default_config(Operation::AudioConverter, &source, "gap.mkv");
        config.format = OutputFormat::Wav;
        config.audio_codec = AudioCodec::Pcm;
        config.audio_adjustments = Some(AudioAdjustments {
            speed_milli: 2000,
            preserve_pitch: false,
            ..Default::default()
        });
        let mut accepted = plan(&source, &config).unwrap();
        let filters = accepted.streams[0].audio_filters.as_ref().unwrap();
        assert_eq!(
            filters.timing_tolerance_us(&source.streams[0]).unwrap(),
            Some(500)
        );
        let mut output = source.clone();
        output.container = "wav".into();
        output.duration_us = Some(2_000_167);
        output.streams[0].duration_us = output.duration_us;
        output.streams[0].time_base = Some(crate::Rational {
            numerator: 1,
            denominator: 48000,
        });
        assert!(verify_output(&output, &source, &accepted).is_ok());
        output.duration_us = Some(2_001_000);
        output.streams[0].duration_us = output.duration_us;
        assert!(verify_output(&output, &source, &accepted).is_err());
        accepted.streams[0]
            .audio_filters
            .as_mut()
            .unwrap()
            .apply_measurement(96008, 48000)
            .unwrap();
        let filters = accepted.streams[0].audio_filters.as_ref().unwrap();
        assert_eq!(
            filters.timing_tolerance_us(&source.streams[0]).unwrap(),
            Some(0)
        );
        output.duration_us = Some(2_000_667);
        output.streams[0].duration_us = output.duration_us;
        assert!(
            verify_output(&output, &source, &accepted).is_err(),
            "A measured endpoint must not inherit source quantization tolerance"
        );
    }

    #[test]
    fn speed_target_budget_and_internal_multitrack_timing_preserve_cached_intent() {
        let mut source = audio_source();
        source.streams[0].codec_id = sys::AV_CODEC_ID_PCM_S16LE as i32;
        source.streams[0].audio.as_mut().unwrap().sample_rate = Some(48000);
        source.streams[0].time_base = Some(crate::Rational {
            numerator: 1,
            denominator: 48000,
        });
        let mut config = default_config(Operation::AudioConverter, &source, "timed.wav");
        config.format = OutputFormat::M4a;
        config.audio_codec = AudioCodec::Aac;
        config.compression = Some(CompressionSettings {
            reduce_file_size: true,
            mode: CompressionMode::TargetSize,
            target: Some(TargetSize {
                amount_milli: 1000,
                unit: TargetSizeUnit::Megabytes,
            }),
            ..Default::default()
        });
        let mut bitrates = Vec::new();
        for speed_milli in [500, 2000] {
            config.audio_adjustments = Some(AudioAdjustments {
                speed_milli,
                ..Default::default()
            });
            let intent = serde_json::to_value(&config).unwrap();
            let accepted = plan(&source, &config).unwrap();
            assert_eq!(serde_json::to_value(&config).unwrap(), intent);
            bitrates.push(
                accepted.streams[0]
                    .encoding
                    .as_ref()
                    .unwrap()
                    .bit_rate
                    .unwrap(),
            );
            assert_eq!(
                accepted.streams[0]
                    .audio_filters
                    .as_ref()
                    .unwrap()
                    .timing()
                    .unwrap()
                    .output_duration_us,
                Some(60_000_000 * 1000 / u64::from(speed_milli))
            );
        }
        assert!(
            bitrates[1] >= bitrates[0] * 3,
            "Target allocation ignored speed-adjusted duration: {bitrates:?}"
        );
        let mut second = source.streams[0].clone();
        second.index = 1;
        second.id = "fixture:1".into();
        second.start_time_us = Some(10_500_000);
        second.duration_us = Some(20_000_000);
        source.streams.push(second);
        // Internal prerequisite only: current Audio Converter acceptance still
        // rejects multiple audio streams, independently of this timing helper.
        assert!(validate_operation(&source, Operation::AudioConverter).is_err());
        let second_timing = audio_timing(
            &source,
            &source.streams[1],
            config.audio_adjustments.as_ref(),
        )
        .unwrap();
        assert_eq!(second_timing.output_duration_us, Some(10_000_000));
        assert_eq!(second_timing.output_start_us, 250_000);
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
