use super::{encoding::EncodingSettings, Plan, StreamPlan};
use crate::{configuration::*, EngineError, MediaInfo, StreamInfo};

pub(super) fn video_codec(name: &str) -> Result<VideoCodec, EngineError> {
    [
        VideoCodec::H264,
        VideoCodec::Hevc,
        VideoCodec::Av1,
        VideoCodec::Vp8,
        VideoCodec::Vp9,
        VideoCodec::Mpeg4,
        VideoCodec::Ffv1,
        VideoCodec::Mjpeg,
    ]
    .into_iter()
    .find(|codec| codec.codec_name() == Some(name))
    .ok_or_else(|| invalid("Choose an implemented video encoder for compression."))
}

fn audio_codec(profile: &EncodingSettings) -> Result<AudioCodec, EngineError> {
    if profile.codec.starts_with("pcm_") {
        return Ok(AudioCodec::Pcm);
    }
    if profile.encoder == "libfdk_aac"
        && profile
            .options
            .iter()
            .any(|option| option.key == "profile" && option.value == "aac_he")
    {
        return Ok(AudioCodec::HeAac);
    }
    [
        AudioCodec::Aac,
        AudioCodec::Mp3,
        AudioCodec::Opus,
        AudioCodec::Vorbis,
        AudioCodec::Flac,
        AudioCodec::Alac,
        AudioCodec::Ac3,
        AudioCodec::Eac3,
        AudioCodec::Wavpack,
        AudioCodec::Wma,
        AudioCodec::AmrNb,
        AudioCodec::AmrWb,
    ]
    .into_iter()
    .find(|codec| codec.codec_name() == Some(profile.codec.as_str()))
    .ok_or_else(|| invalid("Choose an implemented audio encoder for compression."))
}

fn media(item: &StreamPlan, profile: &EncodingSettings) -> Result<CompressionMedia, EngineError> {
    if let Some(rate) = item.sample_rate {
        Ok(CompressionMedia::Audio {
            codec: audio_codec(profile)?,
            sample_rate: rate,
            channels: item
                .channels
                .ok_or_else(|| invalid("The accepted channel count is unknown."))?,
        })
    } else {
        Ok(CompressionMedia::Video {
            codec: video_codec(&profile.codec)?,
        })
    }
}

fn lossy(item: &StreamPlan) -> bool {
    item.encoding.as_ref().is_some_and(|profile| {
        !profile.codec.starts_with("pcm_")
            && !matches!(profile.codec.as_str(), "ffv1" | "flac" | "alac" | "wavpack")
    })
}

pub(super) fn effective_config(
    info: &MediaInfo,
    config: &ConversionConfig,
) -> Result<ConversionConfig, EngineError> {
    let mut effective = config.clone();
    let Some(compression) = config
        .compression
        .as_ref()
        .filter(|settings| settings.reduce_file_size)
    else {
        return Ok(effective);
    };
    if compression.optimize_resolution {
        effective.video_size = VideoSize::P1080;
    }
    if compression.optimize_frame_rate {
        let source = info
            .streams
            .iter()
            .filter_map(|stream| stream.video.as_ref())
            .find(|video| !video.attached_picture)
            .ok_or_else(|| invalid("Frame-rate optimization requires a video source."))?;
        let rate = source.nominal_frame_rate.as_ref().or(source.average_frame_rate.as_ref())
            .filter(|rate| rate.numerator > 0 && rate.denominator > 0)
            .ok_or_else(|| invalid("The source rate is unknown. Choose an explicit frame rate instead of automatic optimization."))?;
        if i64::from(rate.numerator) > 30 * i64::from(rate.denominator) {
            effective.frame_rate = FrameRate::Fps30;
        }
    }
    Ok(effective)
}

pub(super) fn force_video(
    stream: &StreamInfo,
    format: OutputFormat,
    config: &ConversionConfig,
) -> Result<bool, EngineError> {
    let Some(compression) = config
        .compression
        .as_ref()
        .filter(|settings| settings.reduce_file_size)
    else {
        return Ok(false);
    };
    if compression.mode == CompressionMode::Custom && compression.custom_video.is_none() {
        return Ok(false);
    }
    if config.video_codec == VideoCodec::Copy {
        if compression.mode == CompressionMode::TargetSize
            || stream.codec == "ffv1" && compression.mode != CompressionMode::Custom
        {
            return Ok(false);
        }
        if compression.mode == CompressionMode::Custom && compression.custom_video.is_none() {
            return Ok(false);
        }
        return Err(invalid("Stream copying cannot apply video compression controls. Choose Automatic or an applicable encoder."));
    }
    if config.video_codec == VideoCodec::Automatic
        && stream.codec == "ffv1"
        && compression.mode != CompressionMode::Custom
    {
        return Ok(false);
    }
    let facts = stream
        .video
        .as_ref()
        .ok_or_else(|| invalid("The video properties are unknown."))?;
    let profile = video_encoding_settings(
        format,
        config.video_codec,
        facts,
        &stream.codec,
        config.quality,
    )?;
    let kind = CompressionMedia::Video {
        codec: video_codec(&profile.codec)?,
    };
    let policy = if compression.mode == CompressionMode::TargetSize {
        compression_copy_policy(kind, compression)?
    } else {
        apply_compression(&profile, kind, compression, None)?.copy_policy
    };
    Ok(matches!(
        policy,
        CompressionCopyPolicy::EncodeLossy | CompressionCopyPolicy::ReencodeLosslessEffort
    ))
}

pub(super) fn validate_audio_copy(
    stream: &StreamInfo,
    config: &ConversionConfig,
) -> Result<(), EngineError> {
    if config.audio_codec != AudioCodec::Copy {
        return Ok(());
    }
    let Some(settings) = config
        .compression
        .as_ref()
        .filter(|settings| settings.reduce_file_size)
    else {
        return Ok(());
    };
    let uncompressed_or_lossless = stream.codec.starts_with("pcm_")
        || matches!(stream.codec.as_str(), "flac" | "alac" | "wavpack");
    if settings.mode == CompressionMode::TargetSize
        || settings.mode == CompressionMode::Custom && settings.custom_audio.is_none()
        || uncompressed_or_lossless && settings.mode != CompressionMode::Custom
    {
        return Ok(());
    }
    Err(invalid("Stream copying cannot apply audio compression controls. Choose Automatic or an applicable encoder."))
}

fn receiver(plan: &Plan) -> Option<usize> {
    plan.streams
        .iter()
        .position(|item| item.width.is_some() && lossy(item))
        .or_else(|| {
            plan.streams
                .iter()
                .position(|item| item.sample_rate.is_some() && lossy(item))
        })
}

pub(super) fn apply_to_plan(source: &MediaInfo, plan: &mut Plan) -> Result<(), EngineError> {
    let Some(settings) = plan.config.compression.clone() else {
        return Ok(());
    };
    let target = if settings.reduce_file_size && settings.mode == CompressionMode::TargetSize {
        settings.target
    } else {
        None
    };
    plan.target_size_bytes = target.map(TargetSize::bytes).transpose()?;
    let receiver = receiver(plan);
    for (position, item) in plan.streams.iter_mut().enumerate() {
        let Some(profile) = item.encoding.as_ref() else {
            continue;
        };
        let kind = media(item, profile)?;
        let mut ordinary = settings.clone();
        if target.is_some() {
            ordinary.mode = CompressionMode::HighQuality;
            ordinary.target = None;
            // Reserve every non-receiver lossy audio track at an actual ABR.
            // A Vorbis/MP3 quality number is not an estimated fixed bitrate.
            if Some(position) != receiver {
                if let CompressionMedia::Audio {
                    codec,
                    sample_rate,
                    channels,
                } = kind
                {
                    if lossy(item) {
                        let bitrate =
                            super::encoding::lossy_bitrate(codec, sample_rate, channels, None)?;
                        ordinary.mode = CompressionMode::Custom;
                        ordinary.custom_audio = Some(AudioRateControl::AverageBitrate {
                            bits_per_second: u32::try_from(bitrate)
                                .map_err(|_| invalid("The required audio bitrate is too large."))?,
                        });
                    }
                }
            }
        }
        item.encoding = Some(apply_compression(profile, kind, &ordinary, None)?.encoding);
    }
    let Some(target) = target else {
        return Ok(());
    };
    let Some(receiver) = receiver else {
        if plan.streams.iter().any(|item| item.encoding.is_some()) {
            return Err(invalid("A strict target cannot be guaranteed by newly encoding only PCM or lossless streams. Choose a suitable lossy codec or another compression mode."));
        }
        let bytes = source.byte_size.ok_or_else(|| {
            invalid("A target for unchanged streams requires a known source size.")
        })?;
        if bytes > target.bytes()? {
            return Err(invalid("This target cannot contain the unchanged source streams. Choose a suitable lossy codec or increase the target."));
        }
        // Actual remux bytes remain subject to the final Target Not Met gate.
        return Ok(());
    };
    let budget = budget(source, plan, receiver, target)?;
    let item = &mut plan.streams[receiver];
    let profile = item.encoding.as_ref().unwrap();
    let kind = media(item, profile)?;
    let bitrate = applicable_target_bitrate(kind, budget.average_bitrate)?;
    item.encoding = Some(apply_compression(profile, kind, &settings, Some(bitrate))?.encoding);
    Ok(())
}

fn metadata_bytes(source: &MediaInfo) -> Result<u64, EngineError> {
    let mut total = 0u64;
    let entries = source
        .metadata
        .iter()
        .chain(source.streams.iter().flat_map(|stream| &stream.metadata))
        .chain(source.chapters.iter().flat_map(|chapter| &chapter.metadata));
    for entry in entries {
        total = total
            .checked_add(entry.key.len() as u64)
            .and_then(|value| value.checked_add(entry.value.len() as u64))
            .and_then(|value| value.checked_add(64))
            .ok_or_else(|| invalid("The required metadata budget exceeds supported bounds."))?;
    }
    total
        .checked_add(source.chapters.len() as u64 * 64)
        .ok_or_else(|| invalid("The required chapter budget exceeds supported bounds."))
}

fn budget(
    source: &MediaInfo,
    plan: &Plan,
    receiver: usize,
    target: TargetSize,
) -> Result<TargetBudget, EngineError> {
    let duration = source
        .duration_us
        .filter(|duration| *duration > 0)
        .ok_or_else(|| invalid("Target File Size requires a verifiable positive duration."))?;
    let duration_ms = (duration as u64).div_ceil(1_000);
    let target_bytes = target.bytes()?;
    let mut input = TargetBudgetInput {
        duration_ms,
        retained_streams: Vec::new(),
        other_encoded_streams: Vec::new(),
        artwork_bytes: 0,
        subtitle_bytes: 0,
        metadata_bytes: metadata_bytes(source)?,
        // Explicit conservative allowances, not promises about exact mux bytes.
        container_overhead_bytes: match plan.output()?.format {
            OutputFormat::MpegTs => target_bytes.div_ceil(20).max(4_096),
            OutputFormat::Avi | OutputFormat::Asf | OutputFormat::Wma => {
                target_bytes.div_ceil(50).max(8_192)
            }
            _ => target_bytes.div_ceil(100).max(4_096),
        },
        minimum_bitrate: 1,
    };
    for (position, item) in plan.streams.iter().enumerate() {
        if position == receiver {
            continue;
        }
        let original = source
            .streams
            .iter()
            .find(|stream| stream.index == item.index)
            .ok_or_else(|| invalid("The accepted stream source is missing."))?;
        let stream_duration_ms = original
            .duration_us
            .filter(|duration| *duration > 0)
            .map_or(duration_ms, |duration| {
                (duration as u64).div_ceil(1_000).min(duration_ms)
            });
        if let Some(profile) = &item.encoding {
            let bitrate = profile.bit_rate.filter(|rate| *rate > 0).or_else(|| {
                if profile.codec.starts_with("pcm_") {
                    item.sample_rate.zip(item.channels).zip(profile.bit_depth)
                        .map(|((rate, channels), depth)| u64::from(rate) * u64::from(channels) * u64::from(depth))
                } else { None }
            }).ok_or_else(|| invalid("A required lossless or constant-quality auxiliary stream has no reliable target-size budget. Choose an applicable bitrate/codec or another compression mode."))?;
            input.other_encoded_streams.push(RetainedPayload::Bitrate {
                bits_per_second: bitrate,
                duration_ms: stream_duration_ms,
            });
        } else {
            // Unknown subtitle/artwork or copied payload is never counted as
            // zero. The complete known input is a conservative upper bound;
            // this deliberately refuses targets that cannot be justified yet.
            let payload = if let Some(bitrate) = original.bit_rate.filter(|rate| *rate > 0) {
                RetainedPayload::Bitrate {
                    bits_per_second: bitrate,
                    duration_ms: stream_duration_ms,
                }
            } else {
                RetainedPayload::Bytes(source.byte_size.ok_or_else(|| {
                    invalid("A retained stream has no reliable payload bound for this target.")
                })?)
            };
            input.retained_streams.push(payload);
        }
    }
    target_budget(target, &input)
}

fn applicable_target_bitrate(kind: CompressionMedia, ceiling: u64) -> Result<u64, EngineError> {
    let CompressionMedia::Audio {
        codec,
        sample_rate,
        channels,
    } = kind
    else {
        return if ceiling > 0 {
            Ok(ceiling.min(1_000_000_000))
        } else {
            Err(invalid("The target leaves no usable video bitrate."))
        };
    };
    let ceiling = ceiling.min(u64::from(u32::MAX)) as u32;
    let valid =
        |rate| super::encoding::lossy_bitrate(codec, sample_rate, channels, Some(rate)).is_ok();
    if valid(ceiling) {
        return Ok(u64::from(ceiling));
    }
    if matches!(
        codec,
        AudioCodec::Mp3 | AudioCodec::Ac3 | AudioCodec::AmrNb | AudioCodec::AmrWb
    ) {
        // Candidates are revalidated by the single encoding authority. Custom
        // bitrate controls remain exact; only a target ceiling is rounded down.
        return [
            640_000, 576_000, 512_000, 448_000, 384_000, 320_000, 256_000, 224_000, 192_000,
            160_000, 144_000, 128_000, 112_000, 96_000, 80_000, 64_000, 56_000, 48_000, 40_000,
            32_000, 24_000, 23_850, 23_050, 19_850, 18_250, 16_000, 15_850, 14_250, 12_650, 12_200,
            10_200, 8_850, 8_000, 7_950, 7_400, 6_700, 6_600, 5_900, 5_150, 4_750,
        ]
        .into_iter()
        .find(|rate| *rate <= ceiling && valid(*rate))
        .map(u64::from)
        .ok_or_else(|| {
            invalid("The target budget is below this codec's minimum applicable bitrate.")
        });
    }
    let step = if codec == AudioCodec::Eac3 { 1_000 } else { 1 };
    let ceiling = ceiling / step;
    let baseline = u32::try_from(super::encoding::lossy_bitrate(
        codec,
        sample_rate,
        channels,
        None,
    )?)
    .map_err(|_| invalid("The audio bitrate exceeds supported bounds."))?
        / step;
    if ceiling < baseline {
        if valid(ceiling * step) {
            return Ok(u64::from(ceiling * step));
        }
        return Err(invalid(
            "The target budget is below this codec's minimum applicable bitrate.",
        ));
    }
    let (mut low, mut high) = (baseline, ceiling);
    // At most 32 cheap integer validations, never an encoder-open experiment.
    while low < high {
        let middle = low + (high - low).div_ceil(2);
        if valid(middle * step) {
            low = middle;
        } else {
            high = middle - 1;
        }
    }
    Ok(u64::from(low * step))
}

pub(super) fn corrective_plan(
    source: &MediaInfo,
    plan: &Plan,
    actual_bytes: u64,
) -> Result<Option<Plan>, EngineError> {
    let Some(target_bytes) = plan.target_size_bytes else {
        return Ok(None);
    };
    if actual_bytes <= target_bytes || target_bytes == 0 {
        return Ok(None);
    }
    let Some(settings) = plan.config.compression.as_ref() else {
        return Ok(None);
    };
    let Some(target) = settings.target else {
        return Ok(None);
    };
    let Some(receiver) = receiver(plan) else {
        return Ok(None);
    };
    let allocation = budget(source, plan, receiver, target)?;
    if allocation.target_bytes != target_bytes {
        return Err(EngineError::Recovery { detail: "The saved target limit and immutable compression configuration disagree. The valid first output has been preserved.".into() });
    }
    let item = &plan.streams[receiver];
    let profile = item.encoding.as_ref().unwrap();
    let Some(old) = profile.bit_rate.filter(|rate| *rate > 0) else {
        return Ok(None);
    };
    let actual_variable = actual_bytes
        .saturating_sub(allocation.reserved_bytes)
        .max(1);
    // One measured correction leaves a 5% margin on variable payload. Required
    // retained content and accepted geometry/sampling/codec remain unchanged.
    let ceiling = u128::from(old) * u128::from(allocation.encoded_budget_bytes) * 95
        / (u128::from(actual_variable) * 100);
    let kind = media(item, profile)?;
    let Ok(rate) = applicable_target_bitrate(kind, u64::try_from(ceiling).unwrap_or(u64::MAX))
    else {
        return Ok(None);
    };
    if rate >= old {
        return Ok(None);
    }
    let mut corrected = plan.clone();
    corrected.streams[receiver].encoding =
        Some(apply_compression(profile, kind, settings, Some(rate))?.encoding);
    Ok(Some(corrected))
}
