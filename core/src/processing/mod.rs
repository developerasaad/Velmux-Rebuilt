mod transcode;

use crate::{configuration::*, ffmpeg::sys, EngineError, MediaInfo, StreamKind};
use serde::{Deserialize, Serialize};
use std::ffi::CString;

pub(crate) use transcode::convert;
pub(crate) enum ProcessingProgress {
    Timestamp(i64),
    Finalizing,
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
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Plan {
    pub config: ConversionConfig,
    pub streams: Vec<StreamPlan>,
    #[serde(default)]
    pub preview_range: Option<PreviewRange>,
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
    };
    Ok(Plan {
        config,
        streams,
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
    if !info
        .streams
        .iter()
        .any(|stream| stream.kind == StreamKind::Audio)
        && (!matches!(config.audio_codec, AudioCodec::Automatic | AudioCodec::Copy)
            || config.audio_bitrate.is_some()
            || config.sample_rate.is_some()
            || config.channels.is_some())
    {
        return Err(invalid(
            "This source has no audio stream to apply these audio settings to.",
        ));
    }
    let muxer_name = CString::new(config.format.muxer()).unwrap();
    let muxer =
        unsafe { sys::av_guess_format(muxer_name.as_ptr(), std::ptr::null(), std::ptr::null()) };
    if muxer.is_null() {
        return Err(invalid(
            "The selected output format is unavailable in this build.",
        ));
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
        };
        if stream.kind == StreamKind::Video
            && !stream.video.as_ref().is_some_and(|v| v.attached_picture)
        {
            let video = stream
                .video
                .as_ref()
                .ok_or_else(|| invalid("Video geometry could not be inspected."))?;
            let size_change = match config.video_size {
                VideoSize::Original => false,
                size => {
                    let width = video
                        .width
                        .ok_or_else(|| invalid("The source width is unknown."))?;
                    let height = video
                        .height
                        .ok_or_else(|| invalid("The source height is unknown."))?;
                    let sar = video
                        .sample_aspect_ratio
                        .as_ref()
                        .map_or(1.0, |r| f64::from(r.numerator) / f64::from(r.denominator));
                    if !sar.is_finite() || sar <= 0.0 {
                        return Err(invalid("The source pixel aspect ratio is invalid."));
                    }
                    let display_width = f64::from(width) * sar;
                    let display_height = f64::from(height);
                    let (long, short) = if size == VideoSize::Hd720 {
                        (1280.0, 720.0)
                    } else {
                        (854.0, 480.0)
                    };
                    display_width.max(display_height) > long
                        || display_width.min(display_height) > short
                }
            };
            let transform =
                size_change || config.frame_rate != FrameRate::Original || config.quality != 23;
            if config.video_codec == VideoCodec::Copy && transform {
                return Err(invalid(
                    "The selected video settings require encoding; use Automatic or H.264.",
                ));
            }
            let copy_compatible =
                unsafe { sys::avformat_query_codec(muxer, stream.codec_id as _, 0) } > 0;
            let encode = config.video_codec == VideoCodec::H264
                || (config.video_codec == VideoCodec::Automatic && (transform || !copy_compatible));
            if encode {
                if !stream.decoder_available {
                    return Err(invalid(
                        "This source requires a video decoder that is unavailable.",
                    ));
                }
                if video.bit_depth.is_some_and(|depth| depth > 8)
                    || video.mastering_display.is_some()
                    || video.content_light.is_some()
                    || video
                        .color_transfer
                        .as_deref()
                        .is_some_and(|v| matches!(v, "smpte2084" | "arib-std-b67"))
                {
                    return Err(invalid("The current H.264 encoding route cannot preserve this source's high bit depth or HDR. Use compatible stream copying; HDR conversion will be added later."));
                }
                let width = video
                    .width
                    .ok_or_else(|| invalid("The source width is unknown."))?;
                let height = video
                    .height
                    .ok_or_else(|| invalid("The source height is unknown."))?;
                if width == 0 || height == 0 || width > 16384 || height > 16384 {
                    return Err(invalid(
                        "The source geometry exceeds this processing route's limits.",
                    ));
                }
                let sar = video
                    .sample_aspect_ratio
                    .as_ref()
                    .map(|r| f64::from(r.numerator) / f64::from(r.denominator))
                    .unwrap_or(1.0);
                if !sar.is_finite() || sar <= 0.0 {
                    return Err(invalid("The source pixel aspect ratio is invalid."));
                }
                let factor = match config.video_size {
                    VideoSize::Original => 1.0,
                    size => {
                        let long = if size == VideoSize::Hd720 {
                            1280.0
                        } else {
                            854.0
                        };
                        let short = if size == VideoSize::Hd720 {
                            720.0
                        } else {
                            480.0
                        };
                        let dw = f64::from(width) * sar;
                        let dh = f64::from(height);
                        let (bw, bh) = if dw >= dh {
                            (long, short)
                        } else {
                            (short, long)
                        };
                        (bw / dw).min(bh / dh).min(1.0)
                    }
                };
                let (out_w, out_h) = if factor < 1.0 {
                    (
                        ((f64::from(width) * factor) as u32 / 2) * 2,
                        ((f64::from(height) * factor) as u32 / 2) * 2,
                    )
                } else {
                    (width, height)
                };
                if out_w < 2 || out_h < 2 || out_w % 2 != 0 || out_h % 2 != 0 {
                    return Err(invalid("H.264 requires even dimensions. Choose a supported size or compatible stream copying."));
                }
                item.encoder = Some("libx264".into());
                item.expected_codec = "h264".into();
                item.width = Some(out_w);
                item.height = Some(out_h);
            }
        } else if stream.kind == StreamKind::Audio {
            let audio = stream
                .audio
                .as_ref()
                .ok_or_else(|| invalid("Audio properties could not be inspected."))?;
            let desired = if config.operation == Operation::VideoConverter {
                "aac"
            } else {
                match config.format {
                    OutputFormat::M4a => "aac",
                    OutputFormat::Mp3 => "mp3",
                    OutputFormat::Wav => "pcm_s16le",
                    OutputFormat::Flac => "flac",
                    _ => unreachable!(),
                }
            };
            let encoder = if desired == "mp3" {
                "libmp3lame"
            } else {
                desired
            };
            let transform = config
                .sample_rate
                .is_some_and(|rate| Some(rate) != audio.sample_rate)
                || config.channels.is_some_and(|channels| {
                    Some(channels) != audio.channels
                        || audio.channel_layout.as_deref()
                            != Some(if channels == 1 { "mono" } else { "stereo" })
                })
                || config.audio_bitrate.is_some();
            let compatible =
                unsafe { sys::avformat_query_codec(muxer, stream.codec_id as _, 0) } > 0;
            if config.audio_codec == AudioCodec::Copy && transform {
                return Err(invalid(
                    "The selected audio settings require encoding; choose a compatible encoder.",
                ));
            }
            let explicit_target = config.audio_codec != AudioCodec::Automatic
                && config.audio_codec != AudioCodec::Copy;
            let codec_change = stream.codec != desired
                || (desired == "aac" && explicit_target && stream.profile.as_deref() != Some("LC"));
            let encode = config.audio_codec != AudioCodec::Copy
                && (transform
                    || !compatible
                    || ((config.operation == Operation::AudioConverter || explicit_target)
                        && codec_change));
            if encode {
                if !stream.decoder_available {
                    return Err(invalid(
                        "This source requires an audio decoder that is unavailable.",
                    ));
                }
                let rate = config
                    .sample_rate
                    .or(audio.sample_rate)
                    .ok_or_else(|| invalid("The source sample rate is unknown."))?;
                let channels = config
                    .channels
                    .or(audio.channels)
                    .ok_or_else(|| invalid("The source channel layout is unknown."))?;
                if channels == 0 || channels > 8 || !(8000..=192000).contains(&rate) {
                    return Err(invalid("The selected sampling or channel layout is outside this route's supported range."));
                }
                if desired == "aac"
                    && ![
                        8000, 11025, 12000, 16000, 22050, 24000, 32000, 44100, 48000, 64000, 88200,
                        96000,
                    ]
                    .contains(&rate)
                {
                    return Err(invalid(
                        "AAC cannot use this sample rate. Select 44.1 or 48 kHz.",
                    ));
                }
                if desired == "mp3"
                    && (channels > 2
                        || ![8000, 11025, 12000, 16000, 22050, 24000, 32000, 44100, 48000]
                            .contains(&rate))
                {
                    return Err(invalid("MP3 requires a supported sample rate and Mono or Stereo channels. Choose explicit conversion settings."));
                }
                if desired == "flac" {
                    if audio.bit_depth.is_some_and(|depth| depth > 24) {
                        return Err(invalid("This FLAC encoding route preserves up to 24-bit integer precision. The source requires greater precision; choose another compatible representation."));
                    }
                    // A decoded lossy source may not have a meaningful coded
                    // precision. Use 24-bit FLAC instead of silently choosing 16.
                    item.bit_depth = Some(audio.bit_depth.unwrap_or(24));
                }
                item.encoder = Some(encoder.into());
                item.expected_codec = desired.into();
                item.sample_rate = Some(rate);
                item.channels = Some(channels);
            }
        }
        let target_id = if let Some(encoder) = &item.encoder {
            codec_id(encoder)?
        } else {
            stream.codec_id
        };
        if unsafe { sys::avformat_query_codec(muxer, target_id as _, 0) } <= 0 {
            return Err(invalid(format!("{} stream {} is incompatible with this container. Choose a compatible format; tracks are never silently discarded.",stream.codec,stream.index)));
        }
        streams.push(item);
    }
    Ok(Plan {
        config: config.clone(),
        streams,
        preview_range: None,
    })
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
