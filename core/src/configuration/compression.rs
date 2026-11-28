//! Cheap compression policy over an already resolved encoder profile. This
//! module does not probe media, open encoders, choose geometry or publish files.

use super::{AudioCodec, Operation, VideoCodec};
use crate::{
    processing::encoding::{lossy_bitrate, EncoderOption, EncodingSettings},
    EngineError,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, uniffi::Enum)]
pub enum CompressionMode {
    #[default]
    HighQuality,
    Balanced,
    SmallerFile,
    TargetSize,
    Custom,
}

/// Decimal SI units: KB = 1,000 bytes, MB = 1,000,000 bytes, GB =
/// 1,000,000,000 bytes. These are deliberately not binary KiB/MiB/GiB.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, uniffi::Enum)]
pub enum TargetSizeUnit {
    Kilobytes,
    Megabytes,
    Gigabytes,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, uniffi::Record)]
pub struct TargetSize {
    /// Thousandths of the selected unit: 1.5 MB is amount_milli = 1,500.
    pub amount_milli: u64,
    pub unit: TargetSizeUnit,
}

impl TargetSize {
    pub fn bytes(self) -> Result<u64, EngineError> {
        if self.amount_milli == 0 {
            return Err(invalid("Enter a positive target file size."));
        }
        let bytes_per_milli = match self.unit {
            TargetSizeUnit::Kilobytes => 1,
            TargetSizeUnit::Megabytes => 1_000,
            TargetSizeUnit::Gigabytes => 1_000_000,
        };
        self.amount_milli
            .checked_mul(bytes_per_milli)
            .ok_or_else(|| invalid("The target file size is too large."))
    }

    /// Parse a bounded, locale-independent positive decimal with at most three
    /// fractional places. Native presentation can localize the input first.
    pub fn parse_decimal(value: &str, unit: TargetSizeUnit) -> Result<Self, EngineError> {
        if value.is_empty() || value.len() > 24 || !value.is_ascii() {
            return Err(invalid("Enter a positive decimal target size."));
        }
        let (whole, fraction) = value.split_once('.').unwrap_or((value, ""));
        if whole.is_empty()
            || !whole.bytes().all(|byte| byte.is_ascii_digit())
            || !fraction.bytes().all(|byte| byte.is_ascii_digit())
            || fraction.len() > 3
            || value.ends_with('.')
        {
            return Err(invalid(
                "Use a positive target with at most three decimal places.",
            ));
        }
        let whole: u64 = whole
            .parse()
            .map_err(|_| invalid("The target file size is too large."))?;
        let fraction: u64 = if fraction.is_empty() {
            0
        } else {
            fraction
                .parse::<u64>()
                .map_err(|_| invalid("The target file size is invalid."))?
                * 10_u64.pow(3 - fraction.len() as u32)
        };
        let target = Self {
            amount_milli: whole
                .checked_mul(1_000)
                .and_then(|whole| whole.checked_add(fraction))
                .ok_or_else(|| invalid("The target file size is too large."))?,
            unit,
        };
        target.bytes()?;
        Ok(target)
    }
}

/// A single active control prevents contradictory bitrate/quality settings.
/// Quality numbers belong to their named codec; they are not interchangeable.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, uniffi::Enum)]
pub enum VideoRateControl {
    AverageBitrate { bits_per_second: u64 },
    H264Crf { value: u8 },
    HevcCrf { value: u8 },
    Av1Crf { value: u8 },
    Vp8Cq { value: u8 },
    Vp9Crf { value: u8 },
    Mpeg4Qscale { value: u8 },
    MjpegQscale { value: u8 },
    Ffv1Context { large: bool },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, uniffi::Enum)]
pub enum AudioRateControl {
    AverageBitrate { bits_per_second: u32 },
    Mp3VbrQuality { value: u8 },
    VorbisQuality { value: u8 },
    FlacLevel { value: u8 },
    AlacLevel { value: u8 },
    WavpackLevel { value: u8 },
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, uniffi::Record)]
#[serde(default)]
pub struct CompressionSettings {
    pub reduce_file_size: bool,
    pub mode: CompressionMode,
    pub optimize_resolution: bool,
    pub optimize_frame_rate: bool,
    pub target: Option<TargetSize>,
    pub custom_video: Option<VideoRateControl>,
    pub custom_audio: Option<AudioRateControl>,
}

impl CompressionSettings {
    /// Validate workspace applicability before per-stream application. Audio
    /// tracks in a video job still share that video's compression settings.
    pub(crate) fn validate(&self, operation: Operation) -> Result<(), EngineError> {
        if operation == Operation::AudioConverter
            && (self.optimize_resolution || self.optimize_frame_rate || self.custom_video.is_some())
        {
            return Err(invalid(
                "Video compression controls do not apply to audio-only output.",
            ));
        }
        self.validate_active()
    }

    fn validate_active(&self) -> Result<(), EngineError> {
        // Retain inactive workspace choices when the switch is disabled.
        if !self.reduce_file_size {
            return Ok(());
        }
        match self.mode {
            CompressionMode::TargetSize => {
                self.target
                    .ok_or_else(|| invalid("Enter a target file size."))?
                    .bytes()?;
                if self.custom_video.is_some() || self.custom_audio.is_some() {
                    return Err(invalid(
                        "Target size and custom rate control cannot be active together.",
                    ));
                }
            }
            CompressionMode::Custom => {
                if self.target.is_some() {
                    return Err(invalid(
                        "Custom rate control cannot also request a target file size.",
                    ));
                }
                if self.custom_video.is_none() && self.custom_audio.is_none() {
                    return Err(invalid("Choose an applicable custom compression control."));
                }
            }
            _ if self.target.is_some()
                || self.custom_video.is_some()
                || self.custom_audio.is_some() =>
            {
                return Err(invalid(
                    "Clear inactive target/custom settings before selecting a compression preset.",
                ));
            }
            _ => {}
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CompressionMedia {
    Video {
        codec: VideoCodec,
    },
    Audio {
        codec: AudioCodec,
        sample_rate: u32,
        channels: u32,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CompressionCopyPolicy {
    /// Compression must not prevent an otherwise compatible stream copy.
    PreserveCompatible,
    /// Re-encode this lossy stream to apply accepted compression controls.
    EncodeLossy,
    /// Apply explicit lossless effort even when copying would be compatible.
    /// Recompression does not promise that the result will be smaller.
    ReencodeLosslessEffort,
    /// PCM has no compression-effort or lossy rate-control setting.
    NotApplicable,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AppliedCompression {
    pub encoding: EncodingSettings,
    pub copy_policy: CompressionCopyPolicy,
}

/// All retained bytes must come from the intended output composition, rather
/// than blindly reserving every original stream. Bitrate estimates must have
/// their own effective duration, including shorter auxiliary audio.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RetainedPayload {
    Bytes(u64),
    Bitrate {
        bits_per_second: u64,
        duration_ms: u64,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TargetBudgetInput {
    /// Effective duration of the one lossy stream receiving the remainder.
    pub duration_ms: u64,
    pub retained_streams: Vec<RetainedPayload>,
    /// Reserves for all other encoded streams, not just the first audio track.
    pub other_encoded_streams: Vec<RetainedPayload>,
    pub artwork_bytes: u64,
    pub subtitle_bytes: u64,
    pub metadata_bytes: u64,
    /// Explicit muxer-specific allowance supplied by the caller. No hidden
    /// universal overhead formula or removal of required content is applied.
    pub container_overhead_bytes: u64,
    pub minimum_bitrate: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TargetBudget {
    pub target_bytes: u64,
    pub reserved_bytes: u64,
    pub encoded_budget_bytes: u64,
    pub average_bitrate: u64,
}

fn invalid(detail: impl Into<String>) -> EngineError {
    EngineError::Configuration {
        detail: detail.into(),
    }
}

fn unsupported(detail: impl Into<String>) -> EngineError {
    EngineError::UnsupportedMedia {
        detail: detail.into(),
    }
}

fn payload_bytes(payload: RetainedPayload) -> Result<u128, EngineError> {
    match payload {
        RetainedPayload::Bytes(bytes) => Ok(u128::from(bytes)),
        RetainedPayload::Bitrate {
            bits_per_second,
            duration_ms,
        } => {
            if bits_per_second == 0 || duration_ms == 0 {
                return Err(invalid(
                    "A retained bitrate estimate requires a positive rate and effective duration.",
                ));
            }
            Ok((u128::from(bits_per_second) * u128::from(duration_ms)).div_ceil(8_000))
        }
    }
}

/// A feasible budget is a bitrate allocation, not a promise of exact output
/// bytes. The finalized file still requires measurement and Target Not Met.
pub(crate) fn target_budget(
    target: TargetSize,
    input: &TargetBudgetInput,
) -> Result<TargetBudget, EngineError> {
    let target_bytes = target.bytes()?;
    if input.duration_ms == 0 || input.minimum_bitrate == 0 {
        return Err(invalid("Target size requires a known positive effective duration and an applicable minimum bitrate."));
    }
    if input.retained_streams.len() > 1_024 || input.other_encoded_streams.len() > 1_024 {
        return Err(invalid(
            "The target-size stream budget exceeds supported bounds.",
        ));
    }
    let mut reserved = [
        input.artwork_bytes,
        input.subtitle_bytes,
        input.metadata_bytes,
        input.container_overhead_bytes,
    ]
    .into_iter()
    .map(u128::from)
    .sum::<u128>();
    for payload in input
        .retained_streams
        .iter()
        .chain(&input.other_encoded_streams)
    {
        reserved = reserved
            .checked_add(payload_bytes(*payload)?)
            .ok_or_else(|| invalid("The required stream-size budget is too large."))?;
        if reserved >= u128::from(target_bytes) {
            return Err(invalid("The requested target cannot contain all required streams, artwork, subtitles, metadata and container data. Increase the target or explicitly change the output configuration."));
        }
    }
    if reserved >= u128::from(target_bytes) {
        return Err(invalid("The requested target is smaller than the required non-encoded content and container data."));
    }
    let encoded_budget_bytes = target_bytes - reserved as u64;
    let average_bitrate = u128::from(encoded_budget_bytes) * 8_000 / u128::from(input.duration_ms);
    let average_bitrate = u64::try_from(average_bitrate)
        .map_err(|_| invalid("The target implies a bitrate outside supported bounds."))?;
    if average_bitrate < input.minimum_bitrate {
        return Err(invalid("The requested target leaves insufficient bitrate for the accepted media requirements. Increase the target or explicitly adjust those requirements."));
    }
    Ok(TargetBudget {
        target_bytes,
        reserved_bytes: reserved as u64,
        encoded_budget_bytes,
        average_bitrate,
    })
}

fn lossless(media: CompressionMedia) -> bool {
    matches!(
        media,
        CompressionMedia::Video {
            codec: VideoCodec::Ffv1
        } | CompressionMedia::Audio {
            codec: AudioCodec::Flac | AudioCodec::Alac | AudioCodec::Wavpack,
            ..
        }
    )
}

fn pcm(media: CompressionMedia) -> bool {
    matches!(
        media,
        CompressionMedia::Audio {
            codec: AudioCodec::Pcm | AudioCodec::PcmS16le,
            ..
        }
    )
}

pub(crate) fn compression_copy_policy(
    media: CompressionMedia,
    settings: &CompressionSettings,
) -> Result<CompressionCopyPolicy, EngineError> {
    settings.validate_active()?;
    resolved_codec(media)?;
    if pcm(media) {
        if settings.reduce_file_size
            && settings.mode == CompressionMode::Custom
            && settings.custom_audio.is_some()
        {
            return Err(invalid("PCM has no compression-effort or lossy rate-control setting. Choose a compressed codec explicitly."));
        }
        return Ok(CompressionCopyPolicy::NotApplicable);
    }
    if settings.reduce_file_size && settings.mode == CompressionMode::Custom {
        let applicable = match media {
            CompressionMedia::Video { .. } => settings.custom_video.is_some(),
            CompressionMedia::Audio { .. } => settings.custom_audio.is_some(),
        };
        // A custom audio-only edit must not re-encode compatible video, nor
        // should a custom video edit needlessly re-encode its audio tracks.
        if !applicable {
            return Ok(CompressionCopyPolicy::PreserveCompatible);
        }
    }
    if settings.reduce_file_size
        && settings.mode == CompressionMode::Custom
        && lossless(media)
        && match media {
            CompressionMedia::Video { .. } => settings.custom_video.is_some(),
            CompressionMedia::Audio { .. } => settings.custom_audio.is_some(),
        }
    {
        return Ok(CompressionCopyPolicy::ReencodeLosslessEffort);
    }
    Ok(if !settings.reduce_file_size || lossless(media) {
        CompressionCopyPolicy::PreserveCompatible
    } else {
        CompressionCopyPolicy::EncodeLossy
    })
}

fn resolved_codec(media: CompressionMedia) -> Result<&'static str, EngineError> {
    match media {
        CompressionMedia::Video { codec } => codec.codec_name(),
        CompressionMedia::Audio {
            codec,
            sample_rate,
            channels,
        } => {
            if sample_rate == 0 || !(1..=8).contains(&channels) {
                return Err(invalid(
                    "Compression requires valid accepted audio sampling and channels.",
                ));
            }
            codec.codec_name()
        }
    }
    .ok_or_else(|| invalid("Resolve Automatic or Copy before applying an encoding profile."))
}

fn set_option(profile: &mut EncodingSettings, key: &str, value: impl ToString) {
    profile.options.retain(|option| option.key != key);
    profile.options.push(EncoderOption {
        key: key.into(),
        value: value.to_string(),
    });
}

fn clear_rate_control(profile: &mut EncodingSettings) {
    profile.bit_rate = Some(0);
    profile.qscale = None;
    profile.options.retain(|option| {
        !matches!(
            option.key.as_str(),
            "crf" | "cq" | "qmin" | "qmax" | "b" | "minrate" | "maxrate" | "bufsize"
        )
    });
}

fn bounded(value: u8, minimum: u8, maximum: u8, label: &str) -> Result<u8, EngineError> {
    if !(minimum..=maximum).contains(&value) {
        return Err(invalid(format!(
            "{label} must be between {minimum} and {maximum}."
        )));
    }
    Ok(value)
}

fn video_control(
    profile: &mut EncodingSettings,
    codec: VideoCodec,
    control: &VideoRateControl,
) -> Result<(), EngineError> {
    use VideoRateControl::*;
    clear_rate_control(profile);
    match (codec, control) {
        (VideoCodec::Ffv1, Ffv1Context { large }) => {
            // FFmpeg 7.1.5 ffv1enc.c: range coder and context 0/1 preserve
            // precision, unlike a lossy bitrate/quantizer. Keep CRC/level.
            set_option(profile, "coder", 1);
            set_option(profile, "context", u8::from(*large));
        }
        (VideoCodec::Ffv1, _) => return Err(invalid("FFV1 uses lossless context effort, not lossy rate control.")),
        (_, AverageBitrate { bits_per_second }) if (1..=1_000_000_000).contains(bits_per_second) => {
            profile.bit_rate = Some(*bits_per_second);
        }
        (VideoCodec::H264, H264Crf { value }) => set_option(profile, "crf", bounded(*value, 0, 51, "H.264 CRF")?),
        (VideoCodec::Hevc, HevcCrf { value }) => set_option(profile, "crf", bounded(*value, 0, 51, "HEVC CRF")?),
        (VideoCodec::Av1, Av1Crf { value }) => set_option(profile, "crf", bounded(*value, 0, 63, "AV1 CRF")?),
        (VideoCodec::Vp8, Vp8Cq { value }) => {
            let value = bounded(*value, 0, 63, "VP8 CQ")?;
            set_option(profile, "crf", value);
            set_option(profile, "qmin", value);
            set_option(profile, "qmax", value);
        }
        (VideoCodec::Vp9, Vp9Crf { value }) => set_option(profile, "crf", bounded(*value, 0, 63, "VP9 CRF")?),
        (VideoCodec::Mpeg4, Mpeg4Qscale { value }) | (VideoCodec::Mjpeg, MjpegQscale { value }) => {
            profile.qscale = Some(u32::from(bounded(*value, 2, 31, "Video QScale")?));
        }
        _ => return Err(invalid("This custom video rate control does not apply to the selected codec, or its bitrate is outside supported bounds.")),
    }
    Ok(())
}

fn audio_control(
    profile: &mut EncodingSettings,
    codec: AudioCodec,
    rate: u32,
    channels: u32,
    control: &AudioRateControl,
) -> Result<(), EngineError> {
    use AudioRateControl::*;
    clear_rate_control(profile);
    match (codec, control) {
        (AudioCodec::Mp3, Mp3VbrQuality { value }) => {
            profile.qscale = Some(u32::from(bounded(*value, 0, 9, "MP3 VBR quality")?))
        }
        (AudioCodec::Vorbis, VorbisQuality { value }) => {
            profile.qscale = Some(u32::from(bounded(*value, 0, 10, "Vorbis quality")?))
        }
        (AudioCodec::Flac, FlacLevel { value }) => set_option(
            profile,
            "compression_level",
            bounded(*value, 0, 12, "FLAC compression level")?,
        ),
        (AudioCodec::Alac, AlacLevel { value }) => set_option(
            profile,
            "compression_level",
            bounded(*value, 1, 2, "ALAC compression level")?,
        ),
        (AudioCodec::Wavpack, WavpackLevel { value }) => set_option(
            profile,
            "compression_level",
            bounded(*value, 0, 8, "WavPack compression level")?,
        ),
        (_, AverageBitrate { bits_per_second })
            if !lossless(CompressionMedia::Audio {
                codec,
                sample_rate: rate,
                channels,
            }) && !matches!(codec, AudioCodec::Pcm | AudioCodec::PcmS16le) =>
        {
            profile.bit_rate = Some(lossy_bitrate(
                codec,
                rate,
                channels,
                Some(*bits_per_second),
            )?);
        }
        _ => {
            return Err(invalid(
                "This custom audio rate control does not apply to the selected codec.",
            ))
        }
    }
    Ok(())
}

fn video_preset(codec: VideoCodec, mode: CompressionMode) -> Result<VideoRateControl, EngineError> {
    use VideoRateControl::*;
    let index = match mode {
        CompressionMode::Balanced => 1,
        CompressionMode::SmallerFile => 2,
        _ => 0,
    };
    Ok(match codec {
        VideoCodec::H264 => H264Crf {
            value: [18, 23, 28][index],
        },
        VideoCodec::Hevc => HevcCrf {
            value: [18, 23, 28][index],
        },
        VideoCodec::Av1 => Av1Crf {
            value: [24, 32, 40][index],
        },
        VideoCodec::Vp8 => Vp8Cq {
            value: [10, 20, 30][index],
        },
        VideoCodec::Vp9 => Vp9Crf {
            value: [20, 31, 40][index],
        },
        VideoCodec::Mpeg4 => Mpeg4Qscale {
            value: [2, 4, 7][index],
        },
        VideoCodec::Mjpeg => MjpegQscale {
            value: [2, 5, 9][index],
        },
        VideoCodec::Ffv1 => Ffv1Context { large: index != 0 },
        _ => {
            return Err(invalid(
                "Resolve an implemented video encoder before applying compression.",
            ))
        }
    })
}

fn audio_preset(
    codec: AudioCodec,
    rate: u32,
    channels: u32,
    mode: CompressionMode,
) -> Result<AudioRateControl, EngineError> {
    use AudioRateControl::*;
    let index = match mode {
        CompressionMode::Balanced => 1,
        CompressionMode::SmallerFile => 2,
        _ => 0,
    };
    let mono = channels == 1;
    // These are codec-specific starting policies, not claims of equal quality
    // or calibrated size guarantees. Real-media verification remains required.
    let bits_per_second = match codec {
        AudioCodec::Flac => {
            return Ok(FlacLevel {
                value: [0, 5, 8][index],
            })
        }
        AudioCodec::Alac => {
            return Ok(AlacLevel {
                value: if index == 0 { 1 } else { 2 },
            })
        }
        AudioCodec::Wavpack => {
            return Ok(WavpackLevel {
                value: [1, 2, 3][index],
            })
        }
        AudioCodec::Mp3 => {
            return Ok(Mp3VbrQuality {
                value: [0, 2, 5][index],
            })
        }
        AudioCodec::Vorbis => {
            return Ok(VorbisQuality {
                value: [6, 4, 2][index],
            })
        }
        AudioCodec::Aac => {
            let row = match channels {
                1 => [128_000, 64_000, 48_000],
                2 => [256_000, 128_000, 96_000],
                3..=6 => [512_000, 256_000, 192_000],
                _ => [640_000, 384_000, 256_000],
            };
            row[index].min(rate.saturating_mul(channels).saturating_mul(6))
        }
        AudioCodec::Opus => match channels {
            1 => [96_000, 64_000, 32_000][index],
            2 => [160_000, 96_000, 64_000][index],
            3..=6 => [384_000, 256_000, 192_000][index],
            _ => [512_000, 384_000, 256_000][index],
        },
        AudioCodec::Ac3 => {
            if mono {
                [192_000, 128_000, 96_000][index]
            } else if channels <= 2 {
                [256_000, 192_000, 128_000][index]
            } else {
                [640_000, 448_000, 384_000][index]
            }
        }
        AudioCodec::Eac3 => {
            if mono {
                [256_000, 128_000, 96_000][index]
            } else if channels <= 2 {
                [512_000, 256_000, 160_000][index]
            } else {
                [1_024_000, 640_000, 448_000][index]
            }
        }
        AudioCodec::Wma => (if mono {
            [128_000, 96_000, 64_000][index]
        } else {
            [192_000, 128_000, 96_000][index]
        })
        .min(rate.saturating_mul(channels).saturating_mul(6)),
        AudioCodec::HeAac => {
            if mono {
                [48_000, 32_000, 24_000][index]
            } else {
                [96_000, 64_000, 48_000][index]
            }
        }
        AudioCodec::AmrNb => [12_200, 7_950, 5_900][index],
        AudioCodec::AmrWb => [23_850, 15_850, 8_850][index],
        _ => {
            return Err(invalid(
                "This audio representation has no lossy compression control.",
            ))
        }
    };
    Ok(AverageBitrate { bits_per_second })
}

/// Apply immutable policy to a previously verified profile. Target bitrate is
/// supplied only after target_budget has reserved all other required content.
/// It must be a codec-valid bitrate, not an unchecked budget ceiling (MP3/AMR,
/// for example, use discrete rates). A feasible unchanged PCM/lossless copy can
/// be accepted separately after its complete byte budget is checked; this
/// function rejects attempts to force lossless encoding to a strict bitrate.
/// It never changes sampling, precision, layout, geometry, timing or color.
pub(crate) fn apply_compression(
    profile: &EncodingSettings,
    media: CompressionMedia,
    settings: &CompressionSettings,
    target_bitrate: Option<u64>,
) -> Result<AppliedCompression, EngineError> {
    settings.validate_active()?;
    let copy_policy = compression_copy_policy(media, settings)?;
    let expected = resolved_codec(media)?;
    if if pcm(media) {
        !profile.codec.starts_with("pcm_")
    } else {
        profile.codec != expected
    } {
        return Err(invalid(
            "The accepted codec and resolved compression profile disagree.",
        ));
    }
    let target_mode = settings.reduce_file_size && settings.mode == CompressionMode::TargetSize;
    if target_mode && (lossless(media) || pcm(media)) {
        return Err(unsupported("Strict target-size encoding is unavailable for this PCM/lossless stream. Its content-dependent size cannot be forced with a lossy bitrate; choose a suitable lossy codec or another compression mode explicitly."));
    }
    if !target_mode && target_bitrate.is_some() {
        return Err(invalid(
            "A target bitrate applies only to Target File Size mode.",
        ));
    }
    if pcm(media) {
        if settings.reduce_file_size
            && settings.mode == CompressionMode::Custom
            && settings.custom_audio.is_some()
        {
            return Err(invalid("PCM has no compression-effort or lossy rate-control setting. Choose a compressed codec explicitly."));
        }
        return Ok(AppliedCompression {
            encoding: profile.clone(),
            copy_policy,
        });
    }
    let mode = if settings.reduce_file_size {
        settings.mode
    } else {
        CompressionMode::HighQuality
    };
    let mut encoding = profile.clone();
    match media {
        CompressionMedia::Video { codec } => {
            let control = if target_mode {
                VideoRateControl::AverageBitrate {
                    bits_per_second: target_bitrate.ok_or_else(|| {
                        invalid("A complete target-size budget is required before encoding.")
                    })?,
                }
            } else if mode == CompressionMode::Custom {
                settings
                    .custom_video
                    .clone()
                    .unwrap_or(video_preset(codec, CompressionMode::HighQuality)?)
            } else {
                video_preset(codec, mode)?
            };
            video_control(&mut encoding, codec, &control)?;
        }
        CompressionMedia::Audio {
            codec,
            sample_rate,
            channels,
        } => {
            let control = if target_mode {
                AudioRateControl::AverageBitrate {
                    bits_per_second: u32::try_from(target_bitrate.ok_or_else(|| {
                        invalid("A complete target-size budget is required before encoding.")
                    })?)
                    .map_err(|_| {
                        invalid("The target audio bitrate is outside supported bounds.")
                    })?,
                }
            } else if mode == CompressionMode::Custom {
                settings.custom_audio.clone().unwrap_or(audio_preset(
                    codec,
                    sample_rate,
                    channels,
                    CompressionMode::HighQuality,
                )?)
            } else {
                audio_preset(codec, sample_rate, channels, mode)?
            };
            audio_control(&mut encoding, codec, sample_rate, channels, &control)?;
        }
    }
    Ok(AppliedCompression {
        encoding,
        copy_policy,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(codec: &str) -> EncodingSettings {
        EncodingSettings {
            encoder: codec.into(),
            codec: codec.into(),
            pixel_format: Some("yuv420p10le".into()),
            sample_format: Some("fltp".into()),
            bit_depth: Some(10),
            bit_rate: None,
            qscale: None,
            options: vec![EncoderOption {
                key: "thread-policy".into(),
                value: "bounded".into(),
            }],
            channel_layout: Some("5.1(side)".into()),
            color_space: Some("bt2020nc".into()),
            color_range: Some("tv".into()),
        }
    }

    fn enabled(mode: CompressionMode) -> CompressionSettings {
        CompressionSettings {
            reduce_file_size: true,
            mode,
            ..Default::default()
        }
    }

    fn option<'a>(profile: &'a EncodingSettings, key: &str) -> Option<&'a str> {
        profile
            .options
            .iter()
            .find(|option| option.key == key)
            .map(|option| option.value.as_str())
    }

    #[test]
    fn default_and_wire_defaults_preserve_compatible_copying() {
        let settings: CompressionSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(settings, CompressionSettings::default());
        assert!(
            !settings.reduce_file_size
                && !settings.optimize_resolution
                && !settings.optimize_frame_rate
        );
        let input = profile("h264");
        let result = apply_compression(
            &input,
            CompressionMedia::Video {
                codec: VideoCodec::H264,
            },
            &settings,
            None,
        )
        .unwrap();
        assert_eq!(
            result.copy_policy,
            CompressionCopyPolicy::PreserveCompatible
        );
        assert_eq!(option(&result.encoding, "crf"), Some("18"));
        assert_eq!(result.encoding.pixel_format, input.pixel_format);
        assert_eq!(result.encoding.color_space, input.color_space);
        assert_eq!(input.options.len(), 1);
    }

    #[test]
    fn decimal_targets_are_exact_positive_and_checked() {
        for (value, unit, bytes) in [
            ("1.5", TargetSizeUnit::Megabytes, 1_500_000),
            ("0.001", TargetSizeUnit::Kilobytes, 1),
            ("2", TargetSizeUnit::Gigabytes, 2_000_000_000),
        ] {
            assert_eq!(
                TargetSize::parse_decimal(value, unit)
                    .unwrap()
                    .bytes()
                    .unwrap(),
                bytes
            );
        }
        for value in [
            "",
            "0",
            "-1",
            "1e3",
            "NaN",
            ".5",
            "1.",
            "1.2345",
            "1.2.3",
            " 1",
            "18446744073709551615",
        ] {
            assert!(
                TargetSize::parse_decimal(value, TargetSizeUnit::Megabytes).is_err(),
                "{value}"
            );
        }
        assert!(TargetSize {
            amount_milli: u64::MAX,
            unit: TargetSizeUnit::Gigabytes
        }
        .bytes()
        .is_err());
    }

    #[test]
    fn budget_accounts_for_every_stream_and_required_asset() {
        let input = TargetBudgetInput {
            duration_ms: 10_000,
            retained_streams: vec![
                RetainedPayload::Bytes(100_000),
                RetainedPayload::Bitrate {
                    bits_per_second: 80_000,
                    duration_ms: 5_000,
                },
            ],
            other_encoded_streams: vec![
                RetainedPayload::Bitrate {
                    bits_per_second: 128_000,
                    duration_ms: 10_000,
                },
                RetainedPayload::Bitrate {
                    bits_per_second: 64_000,
                    duration_ms: 10_000,
                },
            ],
            artwork_bytes: 20_000,
            subtitle_bytes: 10_000,
            metadata_bytes: 1_000,
            container_overhead_bytes: 9_000,
            minimum_bitrate: 100_000,
        };
        let result = target_budget(
            TargetSize::parse_decimal("1", TargetSizeUnit::Megabytes).unwrap(),
            &input,
        )
        .unwrap();
        assert_eq!(result.reserved_bytes, 430_000);
        assert_eq!(result.encoded_budget_bytes, 570_000);
        assert_eq!(result.average_bitrate, 456_000);
        let mut impossible = input.clone();
        impossible.minimum_bitrate = 456_001;
        assert!(matches!(
            target_budget(
                TargetSize::parse_decimal("1", TargetSizeUnit::Megabytes).unwrap(),
                &impossible
            ),
            Err(EngineError::Configuration { .. })
        ));
        impossible
            .retained_streams
            .push(RetainedPayload::Bytes(u64::MAX));
        assert!(target_budget(
            TargetSize::parse_decimal("1", TargetSizeUnit::Megabytes).unwrap(),
            &impossible
        )
        .is_err());
    }

    #[test]
    fn budget_rounds_reserved_payload_up_and_encoding_rate_down() {
        let input = TargetBudgetInput {
            duration_ms: 3,
            retained_streams: vec![RetainedPayload::Bitrate {
                bits_per_second: 1,
                duration_ms: 1,
            }],
            other_encoded_streams: vec![],
            artwork_bytes: 0,
            subtitle_bytes: 0,
            metadata_bytes: 0,
            container_overhead_bytes: 0,
            minimum_bitrate: 1,
        };
        let result = target_budget(
            TargetSize {
                amount_milli: 2,
                unit: TargetSizeUnit::Kilobytes,
            },
            &input,
        )
        .unwrap();
        assert_eq!(result.reserved_bytes, 1);
        assert_eq!(result.average_bitrate, 2_666);
    }

    #[test]
    fn video_modes_use_codec_specific_scales_and_keep_representation() {
        for (codec, name, key, values) in [
            (VideoCodec::H264, "h264", "crf", ["18", "23", "28"]),
            (VideoCodec::Av1, "av1", "crf", ["24", "32", "40"]),
            (VideoCodec::Vp9, "vp9", "crf", ["20", "31", "40"]),
        ] {
            let input = profile(name);
            for (mode, expected) in [
                CompressionMode::HighQuality,
                CompressionMode::Balanced,
                CompressionMode::SmallerFile,
            ]
            .into_iter()
            .zip(values)
            {
                let result = apply_compression(
                    &input,
                    CompressionMedia::Video { codec },
                    &enabled(mode),
                    None,
                )
                .unwrap();
                assert_eq!(option(&result.encoding, key), Some(expected));
                assert_eq!(option(&result.encoding, "thread-policy"), Some("bounded"));
                assert_eq!(result.encoding.pixel_format, input.pixel_format);
                assert_eq!(result.encoding.color_range, input.color_range);
                assert_eq!(result.copy_policy, CompressionCopyPolicy::EncodeLossy);
            }
        }
    }

    #[test]
    fn custom_bitrate_removes_contradictory_quantizers_only() {
        let mut input = profile("vp8");
        input.qscale = Some(7);
        for key in ["crf", "qmin", "qmax", "maxrate"] {
            set_option(&mut input, key, 20);
        }
        let settings = CompressionSettings {
            custom_video: Some(VideoRateControl::AverageBitrate {
                bits_per_second: 250_000,
            }),
            ..enabled(CompressionMode::Custom)
        };
        let result = apply_compression(
            &input,
            CompressionMedia::Video {
                codec: VideoCodec::Vp8,
            },
            &settings,
            None,
        )
        .unwrap();
        assert_eq!(result.encoding.bit_rate, Some(250_000));
        assert_eq!(result.encoding.qscale, None);
        assert_eq!(result.encoding.options.len(), 1);
        assert_eq!(input.qscale, Some(7));
        assert!(option(&input, "crf").is_some());
    }

    #[test]
    fn custom_codec_mismatch_ranges_and_audio_only_controls_are_rejected() {
        let settings = CompressionSettings {
            custom_video: Some(VideoRateControl::HevcCrf { value: 20 }),
            ..enabled(CompressionMode::Custom)
        };
        assert!(apply_compression(
            &profile("h264"),
            CompressionMedia::Video {
                codec: VideoCodec::H264
            },
            &settings,
            None
        )
        .is_err());
        assert!(settings.validate(Operation::AudioConverter).is_err());
        let settings = CompressionSettings {
            custom_video: Some(VideoRateControl::H264Crf { value: 52 }),
            ..enabled(CompressionMode::Custom)
        };
        assert!(apply_compression(
            &profile("h264"),
            CompressionMedia::Video {
                codec: VideoCodec::H264
            },
            &settings,
            None
        )
        .is_err());
        let settings = CompressionSettings {
            custom_audio: Some(AudioRateControl::AverageBitrate {
                bits_per_second: 123_456,
            }),
            ..enabled(CompressionMode::Custom)
        };
        assert!(apply_compression(
            &profile("mp3"),
            CompressionMedia::Audio {
                codec: AudioCodec::Mp3,
                sample_rate: 48_000,
                channels: 2
            },
            &settings,
            None
        )
        .is_err());
    }

    #[test]
    fn audio_presets_are_layout_aware_and_custom_bitrate_is_authoritative() {
        for (channels, expected) in [(1, 64_000), (2, 128_000), (6, 256_000), (8, 384_000)] {
            let result = apply_compression(
                &profile("aac"),
                CompressionMedia::Audio {
                    codec: AudioCodec::Aac,
                    sample_rate: 48_000,
                    channels,
                },
                &enabled(CompressionMode::Balanced),
                None,
            )
            .unwrap();
            assert_eq!(result.encoding.bit_rate, Some(expected));
        }
        let settings = CompressionSettings {
            custom_audio: Some(AudioRateControl::AverageBitrate {
                bits_per_second: 32_000,
            }),
            ..enabled(CompressionMode::Custom)
        };
        let result = apply_compression(
            &profile("opus"),
            CompressionMedia::Audio {
                codec: AudioCodec::Opus,
                sample_rate: 48_000,
                channels: 2,
            },
            &settings,
            None,
        )
        .unwrap();
        assert_eq!(result.encoding.bit_rate, Some(32_000));
    }

    #[test]
    fn lossless_effort_preserves_precision_and_rejects_strict_target() {
        let media = CompressionMedia::Audio {
            codec: AudioCodec::Flac,
            sample_rate: 96_000,
            channels: 6,
        };
        let input = profile("flac");
        let result =
            apply_compression(&input, media, &enabled(CompressionMode::SmallerFile), None).unwrap();
        assert_eq!(option(&result.encoding, "compression_level"), Some("8"));
        assert_eq!(result.encoding.bit_depth, input.bit_depth);
        assert_eq!(
            result.copy_policy,
            CompressionCopyPolicy::PreserveCompatible
        );
        let settings = CompressionSettings {
            target: Some(TargetSize::parse_decimal("1", TargetSizeUnit::Megabytes).unwrap()),
            ..enabled(CompressionMode::TargetSize)
        };
        assert!(matches!(
            apply_compression(&input, media, &settings, Some(100_000)),
            Err(EngineError::UnsupportedMedia { .. })
        ));
        let result = apply_compression(
            &profile("pcm_s16le"),
            CompressionMedia::Audio {
                codec: AudioCodec::Pcm,
                sample_rate: 48_000,
                channels: 2,
            },
            &enabled(CompressionMode::Balanced),
            None,
        )
        .unwrap();
        assert_eq!(result.copy_policy, CompressionCopyPolicy::NotApplicable);
    }

    #[test]
    fn explicit_lossless_effort_requires_reencoding_but_presets_preserve_copying() {
        let settings = CompressionSettings {
            custom_audio: Some(AudioRateControl::FlacLevel { value: 12 }),
            ..enabled(CompressionMode::Custom)
        };
        let media = CompressionMedia::Audio {
            codec: AudioCodec::Flac,
            sample_rate: 48_000,
            channels: 2,
        };
        let result = apply_compression(&profile("flac"), media, &settings, None).unwrap();
        assert_eq!(
            result.copy_policy,
            CompressionCopyPolicy::ReencodeLosslessEffort
        );
        assert_eq!(option(&result.encoding, "compression_level"), Some("12"));
        assert_eq!(
            compression_copy_policy(media, &enabled(CompressionMode::SmallerFile)).unwrap(),
            CompressionCopyPolicy::PreserveCompatible
        );
        assert!(compression_copy_policy(
            CompressionMedia::Audio {
                codec: AudioCodec::Pcm,
                sample_rate: 48_000,
                channels: 2
            },
            &settings
        )
        .is_err());
    }

    #[test]
    fn target_requires_budget_and_clears_constant_quality() {
        let mut input = profile("h264");
        set_option(&mut input, "crf", 23);
        let settings = CompressionSettings {
            target: Some(TargetSize::parse_decimal("1", TargetSizeUnit::Megabytes).unwrap()),
            ..enabled(CompressionMode::TargetSize)
        };
        let media = CompressionMedia::Video {
            codec: VideoCodec::H264,
        };
        assert!(apply_compression(&input, media, &settings, None).is_err());
        let result = apply_compression(&input, media, &settings, Some(456_000)).unwrap();
        assert_eq!(result.encoding.bit_rate, Some(456_000));
        assert!(option(&result.encoding, "crf").is_none());
        assert!(apply_compression(
            &input,
            media,
            &CompressionSettings::default(),
            Some(456_000)
        )
        .is_err());
    }
}
