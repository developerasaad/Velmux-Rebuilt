//! Lightweight workspace sizing over inspected facts and shared output policy.
//! No execution plan, source access, encoder session or persistent job is made.
use super::{
    apply_compression, effective_config, encoder_accepted, invalid, resolve_audio_codec,
    resolve_audio_output, resolve_format, resolve_video_output, source_option, validate_fields,
    video_codec, CompressionMedia, CompressionMode, ConversionConfig,
};
use crate::{
    processing::encoding::EncodingSettings, EngineError, MediaInfo, StreamInfo, StreamKind,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum EstimateUnavailableReason {
    UnknownDuration,
    UnknownStreamPayload,
    ContentDependentEncoding,
    TargetRequiresMeasuredResult,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum EstimatedOutputSize {
    /// Complete source composition is retained, but the engine still remuxes.
    /// This is the original file's size reference, never exact finalized bytes.
    SourceSizeReference {
        bytes: u64,
    },
    /// Sum of every retained stream's known payload model. This deliberately
    /// excludes variable container headers and embedded metadata representation.
    MediaPayloadApproximation {
        bytes: u64,
        uses_duration_derived_pcm: bool,
    },
    Unavailable {
        reason: EstimateUnavailableReason,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct OutputEstimate {
    pub source_bytes: Option<u64>,
    pub requested_target_bytes: Option<u64>,
    pub size: EstimatedOutputSize,
    pub explanation: String,
}

impl EstimateUnavailableReason {
    fn explanation(self) -> &'static str {
        match self {
            Self::UnknownDuration => "A required stream has no known positive duration. Final output size can only be measured after processing.",
            Self::UnknownStreamPayload => "A required stream has no cached payload size or reliable bitrate. Its bytes cannot be omitted from the estimate; final size is measured after processing.",
            Self::ContentDependentEncoding => "Constant-quality and lossless encoding depend on media content. Final size is measured after encoding rather than predicted from a universal quality formula.",
            Self::TargetRequiresMeasuredResult => "The requested target is a size limit, not an estimate. Actual output bytes must be measured after encoding and checked against that limit.",
        }
    }
}

fn duration(source: &MediaInfo, stream: &StreamInfo) -> Option<u64> {
    if let Some(duration) = stream.duration_us.filter(|value| *value > 0) {
        return Some(duration as u64);
    }
    let timed_streams = source
        .streams
        .iter()
        .filter(|stream| stream.kind == StreamKind::Audio || super::primary_video(stream))
        .count();
    // A container duration does not establish the length of each track. Only
    // the sole timed stream with the same known origin may inherit that fact.
    (timed_streams == 1
        && stream
            .start_time_us
            .zip(source.start_time_us)
            .is_some_and(|(stream_start, source_start)| stream_start == source_start))
    .then(|| {
        source
            .duration_us
            .filter(|value| *value > 0)
            .map(|value| value as u64)
    })
    .flatten()
}

fn rate_bytes(bits_per_second: u64, duration_us: u64) -> Result<u64, EngineError> {
    let bytes = (u128::from(bits_per_second) * u128::from(duration_us)).div_ceil(8_000_000);
    u64::try_from(bytes)
        .map_err(|_| invalid("The projected media payload exceeds supported size bounds."))
}

fn pcm_bytes(rate: u32, channels: u32, bits: u32, duration_us: u64) -> Result<u64, EngineError> {
    let samples = (u128::from(rate) * u128::from(duration_us)).div_ceil(1_000_000);
    let bytes_per_sample = u128::from(bits).div_ceil(8);
    let bytes = samples
        .checked_mul(u128::from(channels))
        .and_then(|value| value.checked_mul(bytes_per_sample))
        .ok_or_else(|| invalid("The projected PCM payload exceeds supported size bounds."))?;
    u64::try_from(bytes)
        .map_err(|_| invalid("The projected PCM payload exceeds supported size bounds."))
}

fn encoded_payload(
    profile: &EncodingSettings,
    rate: Option<u32>,
    channels: Option<u32>,
    duration_us: Option<u64>,
) -> Result<Result<(u64, bool), EstimateUnavailableReason>, EngineError> {
    let Some(duration) = duration_us else {
        return Ok(Err(EstimateUnavailableReason::UnknownDuration));
    };
    if profile.codec.starts_with("pcm_") {
        let Some(((rate, channels), bits)) = rate
            .zip(channels)
            .zip(profile.bit_depth)
            .filter(|((rate, channels), bits)| *rate > 0 && *channels > 0 && *bits > 0)
        else {
            return Ok(Err(EstimateUnavailableReason::UnknownStreamPayload));
        };
        return Ok(Ok((pcm_bytes(rate, channels, bits, duration)?, true)));
    }
    let Some(bitrate) = profile.bit_rate.filter(|value| *value > 0) else {
        return Ok(Err(EstimateUnavailableReason::ContentDependentEncoding));
    };
    Ok(Ok((rate_bytes(bitrate, duration)?, false)))
}

fn copied_payload(
    source: &MediaInfo,
    stream: &StreamInfo,
) -> Result<Result<(u64, bool), EstimateUnavailableReason>, EngineError> {
    if stream.kind == StreamKind::Video
        && stream
            .video
            .as_ref()
            .is_some_and(|video| video.attached_picture)
        || !matches!(stream.kind, StreamKind::Video | StreamKind::Audio)
    {
        // Artwork, subtitle and other auxiliary payloads lack cached byte
        // counts. A single packet/frame count is not a byte-size measurement.
        return Ok(Err(EstimateUnavailableReason::UnknownStreamPayload));
    }
    let Some(duration) = duration(source, stream) else {
        return Ok(Err(EstimateUnavailableReason::UnknownDuration));
    };
    if stream.codec.starts_with("pcm_") {
        if let Some(audio) = &stream.audio {
            // Coded PCM storage width can exceed meaningful source precision
            // (for example 24 significant bits in pcm_s32le). Count stored bits.
            let stored_bits = u32::try_from(unsafe {
                crate::ffmpeg::sys::av_get_bits_per_sample(stream.codec_id as _)
            })
            .ok()
            .filter(|bits| *bits > 0);
            if let Some(((rate, channels), bits)) = audio
                .sample_rate
                .zip(audio.channels)
                .zip(stored_bits)
                .filter(|((rate, channels), bits)| *rate > 0 && *channels > 0 && *bits > 0)
            {
                return Ok(Ok((pcm_bytes(rate, channels, bits, duration)?, true)));
            }
        }
    }
    let Some(bitrate) = stream.bit_rate.filter(|rate| *rate > 0) else {
        return Ok(Err(EstimateUnavailableReason::UnknownStreamPayload));
    };
    Ok(Ok((rate_bytes(bitrate, duration)?, false)))
}

pub(crate) fn estimate(
    info: &MediaInfo,
    config: &ConversionConfig,
) -> Result<OutputEstimate, EngineError> {
    // Naming is validated at acceptance. Incomplete filename edits do not
    // change media payload or invalidate a workspace's sizing projection.
    let mut sizing = config.clone();
    sizing.file_name = "media".into();
    let config = &sizing;
    validate_fields(config)?;
    crate::processing::validate_operation(info, config.operation)?;
    let effective = effective_config(info, config)?;
    let format = resolve_format(info, &effective)?;
    let mut explicit = effective.clone();
    explicit.format = format;
    validate_fields(&explicit)?;
    // Reuse the predefined composition/container contract without accepting a
    // request or constructing an execution plan. Tracks are never dropped here.
    let option = source_option(info, config.operation, config.format).ok_or_else(|| {
        invalid("The retained source composition is incompatible with this output representation.")
    })?;
    if info.streams.iter().any(super::primary_video)
        && !option.video_codecs.contains(&config.video_codec)
        || info
            .streams
            .iter()
            .any(|stream| stream.kind == StreamKind::Audio)
            && !option.audio_codecs.contains(&config.audio_codec)
    {
        return Err(invalid(
            "Choose a compatible codec for the retained output tracks.",
        ));
    }
    let requested_target_bytes = config
        .compression
        .as_ref()
        .filter(|settings| {
            settings.reduce_file_size && settings.mode == CompressionMode::TargetSize
        })
        .and_then(|settings| settings.target)
        .map(|target| target.bytes())
        .transpose()?;
    let mut all_copy = true;
    let mut payload = 0u64;
    let mut uses_pcm = false;
    let mut unavailable = None;
    for stream in &info.streams {
        let encoded = if stream.kind == StreamKind::Audio {
            resolve_audio_output(stream, format, &effective)?
                .map(|output| -> Result<_, EngineError> {
                    let codec = resolve_audio_codec(format, effective.audio_codec, stream)?;
                    Ok((
                        output.settings,
                        CompressionMedia::Audio {
                            codec,
                            sample_rate: output.sample_rate,
                            channels: output.channels,
                        },
                        Some(output.sample_rate),
                        Some(output.channels),
                    ))
                })
                .transpose()?
        } else if super::primary_video(stream) {
            resolve_video_output(stream, format, &effective)?
                .map(|output| -> Result<_, EngineError> {
                    let codec = video_codec(&output.settings.codec)?;
                    Ok((
                        output.settings,
                        CompressionMedia::Video { codec },
                        None,
                        None,
                    ))
                })
                .transpose()?
        } else {
            None
        };
        let result = if let Some((profile, media, rate, channels)) = encoded {
            all_copy = false;
            if !encoder_accepted(format, &profile.encoder) {
                return Err(invalid(
                    "The resolved encoding is incompatible with this output representation.",
                ));
            }
            if requested_target_bytes.is_some() {
                unavailable = Some(EstimateUnavailableReason::TargetRequiresMeasuredResult);
                continue;
            }
            let profile = if let Some(settings) = &effective.compression {
                apply_compression(&profile, media, settings, None)?.encoding
            } else {
                profile
            };
            encoded_payload(&profile, rate, channels, duration(info, stream))?
        } else {
            copied_payload(info, stream)?
        };
        match result {
            Ok((bytes, pcm)) => {
                payload = payload.checked_add(bytes).ok_or_else(|| {
                    invalid("The projected combined media payload exceeds supported size bounds.")
                })?;
                uses_pcm |= pcm;
            }
            Err(reason) => {
                unavailable.get_or_insert(reason);
            }
        }
    }
    // Current converters retain every source stream or reject the composition.
    // If future track selection omits any stream, this shortcut must instead use
    // that intended composition's payload facts, never the whole input file.
    let (size, explanation) = if all_copy && info.byte_size.is_some() {
        (EstimatedOutputSize::SourceSizeReference { bytes: info.byte_size.unwrap() },
            "Source-size reference for retaining all original tracks. Processing still remuxes; different container headers and metadata can change finalized bytes.".into())
    } else if let Some(reason) = unavailable {
        (
            EstimatedOutputSize::Unavailable { reason },
            reason.explanation().into(),
        )
    } else {
        (EstimatedOutputSize::MediaPayloadApproximation { bytes: payload, uses_duration_derived_pcm: uses_pcm },
            if uses_pcm { "Approximate media payload, including PCM sample frames derived from inspected duration and output sampling. Container headers and embedded metadata add bytes." }
            else { "Approximate media payload from all retained tracks' bitrates and durations. Container headers and embedded metadata add bytes." }.into())
    };
    Ok(OutputEstimate {
        source_bytes: info.byte_size,
        requested_target_bytes,
        size,
        explanation,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        AudioCodec, AudioPrecision, AudioRateControl, CompressionSettings, Operation, OutputFormat,
        TargetSize, TargetSizeUnit, VideoCodec, VideoRateControl,
    };
    use serde_json::{json, Value};

    fn video() -> Value {
        json!({"id":"cached:0:0","index":0,"container_id":0,"kind":"Video",
            "codec_id":crate::ffmpeg::sys::AV_CODEC_ID_H264,"codec":"h264","decoder_available":true,
            "duration_us":2000000,"start_time_us":0,"bit_rate":800000,"disposition":0,"metadata":[],
            "video":{"width":320,"height":180,"pixel_format":"yuv420p","bit_depth":8,
                "display_matrix":[],"attached_picture":false,
                "nominal_frame_rate":{"numerator":24,"denominator":1}}})
    }
    fn audio(index: u32, codec: &str, duration: i64) -> Value {
        json!({"id":format!("cached:{index}:{index}"),"index":index,"container_id":index,"kind":"Audio",
            "codec_id":match codec { "aac"=>crate::ffmpeg::sys::AV_CODEC_ID_AAC,
                "pcm_s32le"=>crate::ffmpeg::sys::AV_CODEC_ID_PCM_S32LE,
                _=>crate::ffmpeg::sys::AV_CODEC_ID_PCM_S16LE },
            "codec":codec,"decoder_available":true,"duration_us":duration,"start_time_us":0,
            "frame_count":99999,"disposition":0,"metadata":[],"profile":if codec=="aac" {Some("LC")} else {None},
            "audio":{"sample_rate":48000,"channels":2,"channel_layout":"stereo",
                "sample_format":if codec=="pcm_s32le" {"s32"} else {"s16"},
                "bit_depth":if codec=="pcm_s32le" {24} else {16},
                "initial_padding":0,"trailing_padding":0,"seek_preroll":0}})
    }
    fn source(streams: Vec<Value>) -> MediaInfo {
        serde_json::from_value(
            json!({"source_identity":"cached","byte_size":500000,"container":"matroska",
            "duration_us":2000000,"streams":streams,"metadata":[],"chapters":[],"staged":false}),
        )
        .unwrap()
    }
    fn config() -> ConversionConfig {
        serde_json::from_value(json!({"operation":"VideoConverter","format":"Matroska","video_codec":"Copy",
            "audio_codec":"Aac","video_size":"Original","frame_rate":"Original","quality":23,"file_name":"media"})).unwrap()
    }
    fn custom(
        video: Option<VideoRateControl>,
        audio: Option<AudioRateControl>,
    ) -> CompressionSettings {
        CompressionSettings {
            reduce_file_size: true,
            mode: CompressionMode::Custom,
            custom_video: video,
            custom_audio: audio,
            ..Default::default()
        }
    }

    #[test]
    fn averages_sum_all_tracks_using_each_effective_duration() {
        let source = source(vec![
            video(),
            audio(1, "aac", 2000000),
            audio(2, "aac", 500000),
        ]);
        let mut config = config();
        config.compression = Some(custom(
            None,
            Some(AudioRateControl::AverageBitrate {
                bits_per_second: 128000,
            }),
        ));
        let facts = serde_json::to_value(&source).unwrap();
        let intent = serde_json::to_value(&config).unwrap();
        let result = estimate(&source, &config).unwrap();
        assert_eq!(
            result.size,
            EstimatedOutputSize::MediaPayloadApproximation {
                bytes: 240000,
                uses_duration_derived_pcm: false
            }
        );
        assert!(result.explanation.contains("Container headers"));
        assert_eq!(serde_json::to_value(&source).unwrap(), facts);
        assert_eq!(serde_json::to_value(&config).unwrap(), intent);
    }

    #[test]
    fn pcm_projection_uses_output_sampling_layout_and_stored_precision() {
        let source = source(vec![audio(0, "pcm_s16le", 2000000)]);
        let mut config = config();
        config.operation = Operation::AudioConverter;
        config.format = OutputFormat::Wav;
        config.video_codec = VideoCodec::Automatic;
        config.audio_codec = AudioCodec::Pcm;
        config.sample_rate = Some(44100);
        config.channels = Some(1);
        config.audio_precision = Some(AudioPrecision::Pcm24);
        assert_eq!(
            estimate(&source, &config).unwrap().size,
            EstimatedOutputSize::MediaPayloadApproximation {
                bytes: 264600,
                uses_duration_derived_pcm: true
            }
        );
        // nb_frames=99999 is deliberately unrelated to the audio sample count.
        config.audio_precision = Some(AudioPrecision::Float64);
        assert_eq!(
            estimate(&source, &config).unwrap().size,
            EstimatedOutputSize::MediaPayloadApproximation {
                bytes: 705600,
                uses_duration_derived_pcm: true
            }
        );
        let source = source_with_stored_32_bit_pcm();
        let mut config = self::config();
        config.audio_codec = AudioCodec::Copy;
        config.video_codec = VideoCodec::H264;
        config.compression = Some(custom(
            Some(VideoRateControl::AverageBitrate {
                bits_per_second: 800000,
            }),
            None,
        ));
        assert_eq!(
            estimate(&source, &config).unwrap().size,
            EstimatedOutputSize::MediaPayloadApproximation {
                bytes: 968000,
                uses_duration_derived_pcm: true
            }
        );
    }

    fn source_with_stored_32_bit_pcm() -> MediaInfo {
        source(vec![video(), audio(1, "pcm_s32le", 2000000)])
    }

    #[test]
    fn content_dependent_and_unknown_required_payloads_stay_unavailable() {
        let source = source(vec![video()]);
        let mut config = config();
        config.video_codec = VideoCodec::H264;
        config.audio_codec = AudioCodec::Automatic;
        assert_eq!(
            estimate(&source, &config).unwrap().size,
            EstimatedOutputSize::Unavailable {
                reason: EstimateUnavailableReason::ContentDependentEncoding
            }
        );
        let mut with_subtitles = source.clone();
        with_subtitles.streams.push(serde_json::from_value(json!({"id":"cached:1:1","index":1,"container_id":1,
            "kind":"Subtitle","codec_id":crate::ffmpeg::sys::AV_CODEC_ID_SUBRIP,"codec":"subrip",
            "decoder_available":true,"disposition":0,"metadata":[],"subtitle":{"representation":"Text"}})).unwrap());
        config.compression = Some(custom(
            Some(VideoRateControl::AverageBitrate {
                bits_per_second: 800000,
            }),
            None,
        ));
        assert_eq!(
            estimate(&with_subtitles, &config).unwrap().size,
            EstimatedOutputSize::Unavailable {
                reason: EstimateUnavailableReason::UnknownStreamPayload
            }
        );
        let source = self::source(vec![audio(0, "pcm_s16le", 2000000)]);
        config.operation = Operation::AudioConverter;
        config.format = OutputFormat::Flac;
        config.video_codec = VideoCodec::Automatic;
        config.audio_codec = AudioCodec::Flac;
        config.compression = None;
        assert_eq!(
            estimate(&source, &config).unwrap().size,
            EstimatedOutputSize::Unavailable {
                reason: EstimateUnavailableReason::ContentDependentEncoding
            }
        );
    }

    #[test]
    fn remux_reference_and_requested_target_never_claim_finalized_bytes() {
        let mut source = source(vec![video(), audio(1, "aac", 2000000)]);
        source.container = "mov,mp4,m4a,3gp,3g2,mj2".into();
        source.metadata.push(crate::MetadataEntry {
            key: "major_brand".into(),
            value: "isom".into(),
        });
        let mut config = config();
        config.format = OutputFormat::Original;
        config.audio_codec = AudioCodec::Automatic;
        config.video_codec = VideoCodec::Automatic;
        let reference = estimate(&source, &config).unwrap();
        assert_eq!(
            reference.size,
            EstimatedOutputSize::SourceSizeReference { bytes: 500000 }
        );
        assert!(reference.explanation.contains("still remuxes"));
        config.video_codec = VideoCodec::H264;
        config.compression = Some(CompressionSettings {
            reduce_file_size: true,
            mode: CompressionMode::TargetSize,
            target: Some(TargetSize {
                amount_milli: 600000,
                unit: TargetSizeUnit::Kilobytes,
            }),
            ..Default::default()
        });
        let result = estimate(&source, &config).unwrap();
        assert_eq!(result.requested_target_bytes, Some(600000));
        assert_eq!(result.source_bytes, Some(500000));
        assert_eq!(
            result.size,
            EstimatedOutputSize::Unavailable {
                reason: EstimateUnavailableReason::TargetRequiresMeasuredResult
            }
        );
        // Current Audio Converter does not silently estimate an extraction or
        // reuse complete video-file bytes as an audio-only result.
        config.operation = Operation::AudioConverter;
        assert!(estimate(&source, &config).is_err());
    }

    #[test]
    fn invalid_intent_and_size_overflow_are_rejected_without_execution() {
        assert!(rate_bytes(u64::MAX, u64::MAX).is_err());
        assert!(pcm_bytes(u32::MAX, u32::MAX, u32::MAX, u64::MAX).is_err());
        let source = source(vec![video(), audio(1, "aac", 2000000)]);
        let mut config = config();
        config.sample_rate = Some(12345);
        assert!(matches!(
            estimate(&source, &config),
            Err(EngineError::Configuration { .. })
        ));
    }

    #[test]
    fn container_duration_does_not_invent_unknown_track_lengths() {
        let mut source = source(vec![
            video(),
            audio(1, "aac", 2000000),
            audio(2, "aac", 500000),
        ]);
        source.streams[1].duration_us = None;
        let mut config = config();
        config.compression = Some(custom(
            None,
            Some(AudioRateControl::AverageBitrate {
                bits_per_second: 128000,
            }),
        ));
        assert_eq!(
            estimate(&source, &config).unwrap().size,
            EstimatedOutputSize::Unavailable {
                reason: EstimateUnavailableReason::UnknownDuration
            }
        );
        let mut sole = self::source(vec![audio(0, "pcm_s16le", 2000000)]);
        sole.streams[0].duration_us = None;
        sole.start_time_us = Some(0);
        assert_eq!(duration(&sole, &sole.streams[0]), Some(2000000));
        sole.streams[0].start_time_us = Some(500000);
        assert_eq!(duration(&sole, &sole.streams[0]), None);
    }

    #[test]
    fn filename_edits_do_not_change_sizing_or_mutate_user_intent() {
        let source = source(vec![video(), audio(1, "aac", 2000000)]);
        let mut config = config();
        config.audio_codec = AudioCodec::Copy;
        let expected = estimate(&source, &config).unwrap();
        for name in ["", "../original", "invalid/name", " padded "] {
            config.file_name = name.into();
            let original = serde_json::to_value(&config).unwrap();
            assert_eq!(estimate(&source, &config).unwrap(), expected);
            assert_eq!(serde_json::to_value(&config).unwrap(), original);
            assert!(super::super::validate_fields(&config).is_err());
        }
    }
}
