use crate::{
    ffmpeg::{encoders, sys},
    AudioCodec, AudioInfo, AudioPrecision, EngineError, OutputFormat, VideoCodec, VideoInfo,
};
use serde::{Deserialize, Serialize};
use std::ffi::CString;

/// Resolved software settings, persisted with the accepted stream plan. These
/// library declarations do not establish successful native hardware execution.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct EncodingSettings {
    pub encoder: String,
    pub codec: String,
    pub pixel_format: Option<String>,
    pub sample_format: Option<String>,
    pub bit_depth: Option<u32>,
    pub bit_rate: Option<u64>,
    /// The caller applies AV_CODEC_FLAG_QSCALE and FF_QP2LAMBDA units.
    pub qscale: Option<u32>,
    pub options: Vec<EncoderOption>,
    #[serde(default)]
    pub channel_layout: Option<String>,
    #[serde(default)]
    pub color_space: Option<String>,
    #[serde(default)]
    pub color_range: Option<String>,
}

/// Apply to the AVCodecContext with AV_OPT_SEARCH_CHILDREN: some options are
/// generic context fields, while others belong to the encoder's private data.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct EncoderOption {
    pub key: String,
    pub value: String,
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

fn options(values: &[(&str, &str)]) -> Vec<EncoderOption> {
    values
        .iter()
        .map(|(key, value)| EncoderOption {
            key: (*key).into(),
            value: (*value).into(),
        })
        .collect()
}

fn settings(encoder: &str, codec: &str) -> EncodingSettings {
    EncodingSettings {
        encoder: encoder.into(),
        codec: codec.into(),
        pixel_format: None,
        sample_format: None,
        bit_depth: None,
        bit_rate: None,
        qscale: None,
        options: Vec::new(),
        channel_layout: None,
        color_space: None,
        color_range: None,
    }
}

#[derive(Clone, Copy, Debug)]
struct Pixel {
    depth: u32,
    components: u8,
    chroma_w: u8,
    chroma_h: u8,
    rgb: bool,
    alpha: bool,
    floating: bool,
}

fn pixel(name: &str) -> Result<Pixel, EngineError> {
    if name.is_empty() || name.len() > 128 || !name.is_ascii() {
        return Err(invalid("The video pixel representation is invalid."));
    }
    let name = CString::new(name)
        .map_err(|_| invalid("The video pixel representation contains a null character."))?;
    // FFmpeg owns this immutable descriptor. Copy bounded scalar properties;
    // no descriptor pointer or native allocation escapes the call.
    unsafe {
        let format = sys::av_get_pix_fmt(name.as_ptr());
        let descriptor = sys::av_pix_fmt_desc_get(format);
        if descriptor.is_null()
            || (*descriptor).flags & u64::from(sys::AV_PIX_FMT_FLAG_HWACCEL) != 0
        {
            return Err(unsupported(
                "This software route requires a known software pixel representation.",
            ));
        }
        let descriptor = &*descriptor;
        if descriptor.nb_components == 0
            || usize::from(descriptor.nb_components) > descriptor.comp.len()
        {
            return Err(unsupported("The pixel component declaration is invalid."));
        }
        let depth = descriptor.comp[..usize::from(descriptor.nb_components)]
            .iter()
            .map(|component| component.depth)
            .max()
            .filter(|depth| *depth > 0 && *depth <= 64)
            .ok_or_else(|| unsupported("The source pixel precision is unknown."))?
            as u32;
        Ok(Pixel {
            depth,
            components: descriptor.nb_components,
            chroma_w: descriptor.log2_chroma_w,
            chroma_h: descriptor.log2_chroma_h,
            rgb: descriptor.flags & u64::from(sys::AV_PIX_FMT_FLAG_RGB) != 0,
            alpha: descriptor.flags & u64::from(sys::AV_PIX_FMT_FLAG_ALPHA) != 0,
            floating: descriptor.flags & u64::from(sys::AV_PIX_FMT_FLAG_FLOAT) != 0,
        })
    }
}

fn select_pixel(encoder: &str, source: &VideoInfo) -> Result<(String, Pixel, Pixel), EngineError> {
    let original = source.pixel_format.as_deref().ok_or_else(|| {
        unsupported(
            "The source pixel representation is unknown. Compatible copying may remain available.",
        )
    })?;
    let original_properties = pixel(original)?;
    let required_depth = source.bit_depth.unwrap_or(original_properties.depth);
    if required_depth == 0 || required_depth > original_properties.depth {
        return Err(invalid(
            "The inspected video precision and pixel representation disagree.",
        ));
    }
    let formats = encoders::encoder_formats(encoder)?.ok_or_else(|| {
        unsupported(format!(
            "The required {encoder} software encoder is unavailable in this build."
        ))
    })?;
    let declared = if formats.pixel_formats.is_empty() {
        vec![original.to_owned()]
    } else {
        formats.pixel_formats
    };
    let mut candidates = Vec::new();
    for name in declared {
        let candidate = pixel(&name)?;
        if candidate.depth < required_depth
            || original_properties.alpha && !candidate.alpha
            || candidate.floating != original_properties.floating
            || original_properties.components >= 3 && candidate.components < 3
        {
            continue;
        }
        // Prefer the exact representation, then the same color family/chroma,
        // before an explicit codec-required color/subsampling conversion.
        let chroma_loss = if !original_properties.rgb && !candidate.rgb {
            candidate
                .chroma_w
                .saturating_sub(original_properties.chroma_w)
                + candidate
                    .chroma_h
                    .saturating_sub(original_properties.chroma_h)
        } else {
            0
        };
        let score = (
            u8::from(name != original),
            u8::from(candidate.rgb != original_properties.rgb),
            chroma_loss,
            candidate.depth - required_depth,
            u8::from(candidate.alpha != original_properties.alpha),
            name.clone(),
        );
        candidates.push((score, name, candidate));
    }
    candidates.sort_by(|left, right| left.0.cmp(&right.0));
    let (_, name, selected) = candidates.into_iter().next().ok_or_else(|| {
        unsupported(format!("{encoder} cannot preserve this source's {required_depth}-bit representation and alpha. Choose a compatible codec or stream copying; precision and alpha are never silently discarded."))
    })?;
    encoders::require_video_format(encoder, &name)?;
    Ok((name, original_properties, selected))
}

pub(crate) fn video_settings(
    codec: VideoCodec,
    source: &VideoInfo,
    legacy_quality: u8,
) -> Result<EncodingSettings, EngineError> {
    if !(18..=28).contains(&legacy_quality) {
        return Err(invalid(
            "The retained H.264 quality must be between 18 and 28.",
        ));
    }
    if codec != VideoCodec::H264 && legacy_quality != 23 {
        return Err(invalid("The retained H.264 quality setting does not apply to this codec. Reset that setting or choose H.264."));
    }
    let hdr = source.mastering_display.is_some()
        || source.content_light.is_some()
        || source
            .color_transfer
            .as_deref()
            .is_some_and(|transfer| matches!(transfer, "smpte2084" | "arib-std-b67"));
    if hdr
        && !matches!(
            codec,
            VideoCodec::Hevc | VideoCodec::Av1 | VideoCodec::Vp9 | VideoCodec::Ffv1
        )
    {
        return Err(invalid("This codec route cannot preserve the source HDR representation. Choose a compatible HDR codec, compatible copying, or explicitly request HDR-to-SDR conversion."));
    }
    let mut result = match codec {
        VideoCodec::H264 => {
            let rgb = source
                .pixel_format
                .as_deref()
                .map(pixel)
                .transpose()?
                .is_some_and(|pixel| pixel.rgb);
            let encoder = if rgb && encoders::encoder_formats("libx264rgb")?.is_some() {
                "libx264rgb"
            } else {
                "libx264"
            };
            let mut result = settings(encoder, "h264");
            // Preserve accepted M2 H.264 requests exactly. The complete M3
            // compression model replaces this legacy field independently.
            result.options = options(&[("preset", "veryfast")]);
            result.options.push(EncoderOption {
                key: "crf".into(),
                value: legacy_quality.to_string(),
            });
            result.bit_rate = Some(0);
            result
        }
        VideoCodec::Hevc => {
            let mut result = settings("libx265", "hevc");
            result.options = options(&[
                ("preset", "fast"),
                ("crf", "18"),
                ("x265-params", "pools=1:frame-threads=2:log-level=error"),
            ]);
            result.bit_rate = Some(0);
            result
        }
        VideoCodec::Av1 => {
            let mut result = settings("libsvtav1", "av1");
            result.options = options(&[("preset", "8"), ("crf", "24"), ("svtav1-params", "lp=2")]);
            result.bit_rate = Some(0);
            result
        }
        VideoCodec::Vp8 => {
            let mut result = settings("libvpx", "vp8");
            // The pinned VP8 wrapper uses constrained CQ rather than VP9's
            // unconstrained mode. Equal quantizer limits retain this fixed
            // quality policy without a made-up resolution bitrate formula.
            result.options = options(&[
                ("deadline", "good"),
                ("cpu-used", "2"),
                ("crf", "10"),
                ("qmin", "10"),
                ("qmax", "10"),
            ]);
            result.bit_rate = Some(0);
            result
        }
        VideoCodec::Vp9 => {
            let mut result = settings("libvpx-vp9", "vp9");
            result.options = options(&[("deadline", "good"), ("cpu-used", "4"), ("crf", "20")]);
            result.bit_rate = Some(0);
            result
        }
        VideoCodec::Mpeg4 => {
            let mut result = settings("mpeg4", "mpeg4");
            result.qscale = Some(2);
            result.bit_rate = Some(0);
            result
        }
        VideoCodec::Ffv1 => {
            let mut result = settings("ffv1", "ffv1");
            result.options = options(&[
                ("level", "3"),
                ("coder", "1"),
                ("context", "1"),
                ("slicecrc", "1"),
            ]);
            result
        }
        VideoCodec::Mjpeg => {
            let mut result = settings("mjpeg", "mjpeg");
            result.qscale = Some(2);
            result.bit_rate = Some(0);
            result
        }
        VideoCodec::Automatic | VideoCodec::Copy => {
            return Err(invalid(
                "Resolve Automatic or compatible copying before requesting encoder settings.",
            ))
        }
    };
    let (mut selected_name, original, mut selected) = select_pixel(&result.encoder, source)?;
    if codec == VideoCodec::Mjpeg && !selected_name.starts_with("yuvj") {
        // Standard JPEG samples use full range. An explicit MJPEG request
        // therefore needs real range conversion, not relabeled limited pixels.
        let full_range = selected_name.replacen("yuv", "yuvj", 1);
        if encoders::require_video_format(&result.encoder, &full_range).is_ok() {
            selected_name = full_range;
            selected = pixel(&selected_name)?;
        }
    }
    result.color_space = if original.rgb != selected.rgb {
        Some(if selected.rgb { "gbr" } else { "bt709" }.into())
    } else {
        source.color_space.clone()
    };
    result.color_range = if selected_name.starts_with("yuvj") || !original.rgb && selected.rgb {
        Some("pc".into())
    } else if original.rgb && !selected.rgb {
        Some("tv".into())
    } else {
        source.color_range.clone()
    };
    if let Some(space) = &result.color_space {
        let name = CString::new(space.as_str())
            .map_err(|_| invalid("The source color matrix is invalid."))?;
        let matrix = unsafe { sys::av_color_space_from_name(name.as_ptr()) };
        let supported = [
            sys::AVCOL_SPC_UNSPECIFIED,
            sys::AVCOL_SPC_RGB,
            sys::AVCOL_SPC_BT709,
            sys::AVCOL_SPC_BT470BG,
            sys::AVCOL_SPC_SMPTE170M,
            sys::AVCOL_SPC_FCC,
            sys::AVCOL_SPC_SMPTE240M,
            sys::AVCOL_SPC_BT2020_NCL,
        ];
        if !supported.iter().any(|value| matrix == *value as i32) {
            return Err(unsupported("This software scaling route cannot faithfully preserve the source color matrix. Choose compatible copying or another supported representation."));
        }
    }
    result.bit_depth = Some(selected.depth);
    result.pixel_format = Some(selected_name);
    Ok(result)
}

fn validate_layout(channels: u32, layout: &str) -> Result<(), EngineError> {
    if !(1..=8).contains(&channels) || layout.is_empty() || layout.len() > 256 || !layout.is_ascii()
    {
        return Err(invalid(
            "Choose a known Mono, Stereo or supported multichannel layout.",
        ));
    }
    let name =
        CString::new(layout).map_err(|_| invalid("The audio layout contains a null character."))?;
    // The parser may allocate a custom map. Release it on every path, and retain
    // only the validated scalar channel count/order, never native pointers.
    unsafe {
        let mut native: sys::AVChannelLayout = std::mem::zeroed();
        let parsed = sys::av_channel_layout_from_string(&mut native, name.as_ptr());
        let valid = parsed >= 0
            && native.nb_channels == channels as i32
            && native.order == sys::AV_CHANNEL_ORDER_NATIVE
            && sys::av_channel_layout_check(&native) == 1;
        sys::av_channel_layout_uninit(&mut native);
        if !valid {
            return Err(invalid("The selected channel count and named speaker layout disagree or are unspecified. Choose an explicit supported layout."));
        }
    }
    Ok(())
}

fn original_precision(source: &AudioInfo) -> Result<AudioPrecision, EngineError> {
    match source.sample_format.as_deref() {
        Some("flt" | "fltp") => return Ok(AudioPrecision::Float32),
        Some("dbl" | "dblp") => return Ok(AudioPrecision::Float64),
        _ => {}
    }
    let depth = source
        .bit_depth
        .or(match source.sample_format.as_deref() {
            Some("u8" | "u8p") => Some(8),
            Some("s16" | "s16p") => Some(16),
            Some("s32" | "s32p") => Some(32),
            Some("s64" | "s64p") => Some(64),
            _ => None,
        })
        .ok_or_else(|| {
            invalid(
                "The original sample precision is unknown. Select an explicit supported precision.",
            )
        })?;
    match depth {
        1..=8 => Ok(AudioPrecision::Pcm8),
        9..=16 => Ok(AudioPrecision::Pcm16),
        17..=24 => Ok(AudioPrecision::Pcm24),
        25..=32 => Ok(AudioPrecision::Pcm32),
        _ => Err(invalid("This encoder cannot preserve the original integer precision. Choose a compatible representation or an explicit supported precision.")),
    }
}

fn integer_precision(
    source: &AudioInfo,
    precision: AudioPrecision,
    maximum: u32,
) -> Result<u32, EngineError> {
    let bits = match precision {
        AudioPrecision::Original => {
            // Unknown coded precision (for example decoded AAC) does not imply
            // a lossless float source. A declared float original is different.
            if source.bit_depth.is_some()
                && matches!(original_precision(source)?, AudioPrecision::Float32 | AudioPrecision::Float64)
            {
                return Err(invalid("This lossless integer codec cannot preserve the original floating-point representation. Choose PCM/WavPack or explicitly select an integer precision."));
            }
            source.bit_depth.unwrap_or(match source.sample_format.as_deref() {
                Some("u8" | "u8p") => 8,
                Some("s16" | "s16p") => 16,
                Some("s32" | "s32p") => 32,
                Some("s64" | "s64p") => 64,
                // Lossy float decoder output has no coded integer precision.
                // Use the lossless encoder's supported high-fidelity depth.
                _ => 24,
            })
        }
        AudioPrecision::Pcm8 => 8,
        AudioPrecision::Pcm16 => 16,
        AudioPrecision::Pcm24 => 24,
        AudioPrecision::Pcm32 => 32,
        AudioPrecision::Float32 | AudioPrecision::Float64 => return Err(invalid("This codec requires integer PCM precision. Choose an integer precision or a compatible floating-point representation.")),
    };
    if bits == 0 || bits > maximum {
        return Err(invalid(format!("This codec preserves at most {maximum}-bit integer samples. Select an applicable precision explicitly; the source is not silently reduced.")));
    }
    Ok(bits)
}

fn choose_sample(encoder: &str, preferred: &[&str]) -> Result<String, EngineError> {
    let formats = encoders::encoder_formats(encoder)?.ok_or_else(|| {
        unsupported(format!(
            "The required {encoder} software encoder is unavailable in this build."
        ))
    })?;
    preferred
        .iter()
        .find(|sample| {
            formats.sample_formats.is_empty()
                || formats.sample_formats.iter().any(|value| value == **sample)
        })
        .map(|sample| (*sample).to_owned())
        .ok_or_else(|| {
            unsupported(format!(
                "{encoder} cannot encode the accepted sample representation."
            ))
        })
}

fn pcm_settings(
    format: OutputFormat,
    source: &AudioInfo,
    precision: AudioPrecision,
) -> Result<EncodingSettings, EngineError> {
    if precision == AudioPrecision::Original
        && (source
            .sample_format
            .as_deref()
            .is_some_and(|format| matches!(format, "s64" | "s64p"))
            || source.bit_depth.is_some_and(|depth| depth > 32)
                && !source
                    .sample_format
                    .as_deref()
                    .is_some_and(|format| matches!(format, "flt" | "fltp" | "dbl" | "dblp")))
    {
        if source
            .bit_depth
            .is_some_and(|depth| depth == 0 || depth > 64)
        {
            return Err(invalid(
                "The original integer sample precision is outside supported PCM bounds.",
            ));
        }
        let codec = if format == OutputFormat::Aiff {
            "pcm_s64be"
        } else {
            "pcm_s64le"
        };
        let mut result = settings(codec, codec);
        result.sample_format = Some("s64".into());
        result.bit_depth = Some(64);
        return Ok(result);
    }
    let precision = if precision == AudioPrecision::Original {
        original_precision(source)?
    } else {
        precision
    };
    let big_endian = format == OutputFormat::Aiff;
    let (codec, sample, bits) = match (precision, big_endian) {
        (AudioPrecision::Pcm8, false) => ("pcm_u8", "u8", 8),
        (AudioPrecision::Pcm8, true) => ("pcm_s8", "u8", 8),
        (AudioPrecision::Pcm16, false) => ("pcm_s16le", "s16", 16),
        (AudioPrecision::Pcm16, true) => ("pcm_s16be", "s16", 16),
        (AudioPrecision::Pcm24, false) => ("pcm_s24le", "s32", 24),
        (AudioPrecision::Pcm24, true) => ("pcm_s24be", "s32", 24),
        (AudioPrecision::Pcm32, false) => ("pcm_s32le", "s32", 32),
        (AudioPrecision::Pcm32, true) => ("pcm_s32be", "s32", 32),
        (AudioPrecision::Float32, false) => ("pcm_f32le", "flt", 32),
        (AudioPrecision::Float32, true) => ("pcm_f32be", "flt", 32),
        (AudioPrecision::Float64, false) => ("pcm_f64le", "dbl", 64),
        (AudioPrecision::Float64, true) => ("pcm_f64be", "dbl", 64),
        (AudioPrecision::Original, _) => unreachable!("Original precision was resolved above"),
    };
    let mut result = settings(codec, codec);
    result.sample_format = Some(sample.into());
    result.bit_depth = Some(bits);
    Ok(result)
}

pub(crate) fn lossy_bitrate(
    codec: AudioCodec,
    rate: u32,
    channels: u32,
    explicit: Option<u32>,
) -> Result<u64, EngineError> {
    let desired = explicit.unwrap_or(match codec {
        AudioCodec::Aac => match channels {
            1 => 128_000,
            2 => 256_000,
            3..=6 => 512_000,
            _ => 640_000,
        },
        AudioCodec::Mp3 if rate < 16000 => 64_000,
        AudioCodec::Mp3 if rate < 32000 => 160_000,
        AudioCodec::Mp3 => {
            if channels == 1 {
                160_000
            } else {
                256_000
            }
        }
        AudioCodec::Opus => match channels {
            1 => 96_000,
            2 => 160_000,
            3..=6 => 384_000,
            _ => 512_000,
        },
        AudioCodec::Vorbis => 192_000,
        AudioCodec::Ac3 => match channels {
            1 => 192_000,
            2 => 256_000,
            _ => 640_000,
        },
        AudioCodec::Eac3 => match channels {
            1 => 256_000,
            2 => 512_000,
            _ => 1_024_000,
        },
        AudioCodec::Wma => {
            if channels == 1 {
                128_000
            } else {
                192_000
            }
        }
        AudioCodec::HeAac => {
            if channels == 1 {
                48_000
            } else {
                96_000
            }
        }
        AudioCodec::AmrNb => 12_200,
        AudioCodec::AmrWb => 23_850,
        _ => return Err(invalid("A lossy bitrate does not apply to this codec.")),
    });
    let bitrate = if explicit.is_none() && matches!(codec, AudioCodec::Aac | AudioCodec::Wma) {
        desired.min(rate.saturating_mul(channels).saturating_mul(6))
    } else {
        desired
    };
    let valid = match codec {
        AudioCodec::Aac => {
            bitrate >= 8_000 && u64::from(bitrate) <= u64::from(rate) * u64::from(channels) * 6
        }
        AudioCodec::Mp3 => {
            let allowed: &[u32] = if rate < 16000 {
                &[
                    8_000, 16_000, 24_000, 32_000, 40_000, 48_000, 56_000, 64_000,
                ]
            } else if rate < 32000 {
                &[
                    8_000, 16_000, 24_000, 32_000, 40_000, 48_000, 56_000, 64_000, 80_000, 96_000,
                    112_000, 128_000, 144_000, 160_000,
                ]
            } else {
                &[
                    32_000, 40_000, 48_000, 56_000, 64_000, 80_000, 96_000, 112_000, 128_000,
                    160_000, 192_000, 224_000, 256_000, 320_000,
                ]
            };
            allowed.contains(&bitrate)
        }
        AudioCodec::Opus => (500..=256_000u32.saturating_mul(channels)).contains(&bitrate),
        AudioCodec::Vorbis => (16_000..=256_000u32.saturating_mul(channels)).contains(&bitrate),
        AudioCodec::Ac3 => [
            32_000, 40_000, 48_000, 56_000, 64_000, 80_000, 96_000, 112_000, 128_000, 160_000,
            192_000, 224_000, 256_000, 320_000, 384_000, 448_000, 512_000, 576_000, 640_000,
        ]
        .contains(&bitrate),
        AudioCodec::Eac3 => (32_000..=6_144_000).contains(&bitrate) && bitrate.is_multiple_of(1000),
        AudioCodec::Wma => {
            (24_000..=320_000).contains(&bitrate)
                && u64::from(bitrate) <= u64::from(rate) * u64::from(channels) * 6
        }
        AudioCodec::HeAac => (16_000..=128_000).contains(&bitrate),
        AudioCodec::AmrNb => {
            [4_750, 5_150, 5_900, 6_700, 7_400, 7_950, 10_200, 12_200].contains(&bitrate)
        }
        AudioCodec::AmrWb => [
            6_600, 8_850, 12_650, 14_250, 15_850, 18_250, 19_850, 23_050, 23_850,
        ]
        .contains(&bitrate),
        _ => false,
    };
    if !valid {
        return Err(invalid("This bitrate cannot be encoded with the selected codec, sampling and channel layout. Choose an applicable bitrate or Automatic."));
    }
    Ok(u64::from(bitrate))
}

// The complete accepted representation is explicit at this internal boundary.
#[allow(clippy::too_many_arguments)]
pub(crate) fn audio_settings(
    codec: AudioCodec,
    format: OutputFormat,
    source: &AudioInfo,
    rate: u32,
    channels: u32,
    layout: &str,
    precision: AudioPrecision,
    bitrate: Option<u32>,
) -> Result<EncodingSettings, EngineError> {
    if rate == 0 || rate > 768_000 {
        return Err(invalid(
            "The selected audio sample rate is outside this route's bounds.",
        ));
    }
    validate_layout(channels, layout)?;
    let lossless = matches!(
        codec,
        AudioCodec::PcmS16le
            | AudioCodec::Pcm
            | AudioCodec::Flac
            | AudioCodec::Alac
            | AudioCodec::Wavpack
    );
    if lossless && bitrate.is_some() {
        return Err(invalid(
            "A lossy bitrate does not apply to PCM or lossless encoding.",
        ));
    }
    if !lossless && precision != AudioPrecision::Original {
        return Err(invalid("Sample precision applies to PCM and supported lossless representations, not this lossy codec."));
    }
    let mut result = match codec {
        AudioCodec::PcmS16le => {
            if !matches!(precision, AudioPrecision::Original | AudioPrecision::Pcm16) {
                return Err(invalid("The retained PCM 16-bit codec conflicts with the selected sample precision. Choose PCM or 16-bit precision."));
            }
            let mut result = settings("pcm_s16le", "pcm_s16le");
            result.sample_format = Some("s16".into());
            result.bit_depth = Some(16);
            result
        }
        AudioCodec::Pcm => pcm_settings(format, source, precision)?,
        AudioCodec::Flac => {
            let depth = integer_precision(source, precision, 24)?;
            let mut result = settings("flac", "flac");
            result.bit_depth = Some(depth);
            result.sample_format = Some(if depth <= 16 { "s16" } else { "s32" }.into());
            result.options = options(&[("compression_level", "5")]);
            result
        }
        AudioCodec::Alac => {
            let depth = integer_precision(source, precision, 24)?;
            let mut result = settings("alac", "alac");
            result.bit_depth = Some(if depth <= 16 { 16 } else { 24 });
            result.sample_format = Some(if depth <= 16 { "s16p" } else { "s32p" }.into());
            result.options = options(&[("compression_level", "2")]);
            result
        }
        AudioCodec::Wavpack => {
            let selected = if precision == AudioPrecision::Original {
                original_precision(source)?
            } else {
                precision
            };
            let (sample, depth) = match selected {
                AudioPrecision::Pcm8 => ("u8p", 8),
                AudioPrecision::Pcm16 => ("s16p", 16),
                AudioPrecision::Pcm24 => ("s32p", 24),
                AudioPrecision::Pcm32 => ("s32p", 32),
                AudioPrecision::Float32 => ("fltp", 32),
                AudioPrecision::Float64 => return Err(invalid("This WavPack encoder cannot preserve 64-bit floating-point samples. Choose floating-point PCM or explicitly select a supported precision.")),
                AudioPrecision::Original => unreachable!("Original precision was resolved above"),
            };
            let mut result = settings("wavpack", "wavpack");
            result.sample_format = Some(sample.into());
            result.bit_depth = Some(depth);
            result.options = options(&[("compression_level", "2")]);
            result
        }
        AudioCodec::Aac => {
            // The native AAC encoder has no public layout list in 7.1.5.
            // These named configurations come from aacenc.c/aacenctab.h,
            // including distinct side/back arrangements rather than counts.
            if !matches!(
                layout,
                "mono"
                    | "stereo"
                    | "2.1"
                    | "3.0(back)"
                    | "3.0"
                    | "3.1"
                    | "4.0"
                    | "4.1"
                    | "quad(side)"
                    | "quad"
                    | "5.0(side)"
                    | "5.1(side)"
                    | "5.0"
                    | "5.1"
                    | "6.0"
                    | "6.0(front)"
                    | "hexagonal"
                    | "6.1"
                    | "6.1(back)"
                    | "6.1(front)"
                    | "7.0"
                    | "7.0(front)"
                    | "7.1"
                    | "7.1(wide)"
                    | "7.1(wide-side)"
                    | "octagonal"
            ) {
                return Err(invalid("The native AAC encoder cannot represent this named speaker layout. Choose a supported layout or another codec."));
            }
            let mut result = settings("aac", "aac");
            result.options = options(&[("profile", "aac_low")]);
            // Explicit PCEs distinguish the intended side/back speakers and
            // avoid ambiguous interpretation of AAC's eight-channel shortcut.
            if matches!(layout, "5.1(side)" | "7.1") {
                result.options.push(EncoderOption {
                    key: "aac_pce".into(),
                    value: "1".into(),
                });
            }
            result
        }
        AudioCodec::Mp3 => {
            if channels > 2 {
                return Err(invalid("MP3 supports Mono or Stereo. Select an explicit supported layout or another codec."));
            }
            settings("libmp3lame", "mp3")
        }
        AudioCodec::Opus | AudioCodec::Vorbis => {
            // FFmpeg's pinned wrappers expect Vorbis speaker ordering. Accept
            // named layouts, never a same-count reassignment of side/back pairs.
            let allowed = match channels {
                1 => layout == "mono",
                2 => layout == "stereo",
                3 => layout == "3.0",
                4 => layout == "quad",
                5 => layout == "5.0",
                6 => layout == "5.1",
                7 => layout == "6.1",
                8 => layout == "7.1",
                _ => false,
            };
            if !allowed {
                return Err(invalid("This encoder requires a supported Vorbis-order speaker layout. Choose an explicit compatible layout; speakers are never silently reassigned."));
            }
            if codec == AudioCodec::Opus {
                let mut result = settings("libopus", "opus");
                result.options = options(&[
                    ("application", "audio"),
                    ("vbr", "on"),
                    ("mapping_family", if channels > 2 { "1" } else { "0" }),
                ]);
                result
            } else {
                let mut result = settings("libvorbis", "vorbis");
                if bitrate.is_none() {
                    result.qscale = Some(6);
                }
                result
            }
        }
        AudioCodec::Ac3 => settings("ac3", "ac3"),
        AudioCodec::Eac3 => settings("eac3", "eac3"),
        AudioCodec::Wma => {
            if channels > 2 || rate > 48_000 {
                return Err(invalid("WMA encoding supports Mono/Stereo up to 48 kHz. Choose explicit supported settings or compatible copying."));
            }
            settings("wmav2", "wmav2")
        }
        AudioCodec::HeAac => {
            if channels > 2 {
                return Err(invalid("This HE-AAC route supports Mono or Stereo."));
            }
            let mut result = settings("libfdk_aac", "aac");
            result.options = options(&[("profile", "aac_he")]);
            result
        }
        AudioCodec::AmrNb | AudioCodec::AmrWb => {
            let narrow = codec == AudioCodec::AmrNb;
            if channels != 1 || rate != if narrow { 8000 } else { 16000 } {
                return Err(invalid("AMR-NB requires 8 kHz Mono; AMR-WB requires 16 kHz Mono. Select those settings explicitly."));
            }
            if narrow {
                settings("libopencore_amrnb", "amr_nb")
            } else {
                settings("libvo_amrwbenc", "amr_wb")
            }
        }
        AudioCodec::Automatic | AudioCodec::Copy => {
            return Err(invalid(
                "Resolve Automatic or compatible copying before requesting encoder settings.",
            ))
        }
    };
    // Resolve availability before rate policy so an absent conditional encoder
    // cannot be presented as an available software/native capability.
    let declared = encoders::encoder_formats(&result.encoder)?.ok_or_else(|| {
        unsupported(format!(
            "The required {} software encoder is unavailable in this build.",
            result.encoder
        ))
    })?;
    if result.sample_format.is_none() {
        let preferred: &[&str] = match codec {
            AudioCodec::Opus => &["flt", "s16"],
            AudioCodec::AmrNb | AudioCodec::AmrWb => &["s16"],
            AudioCodec::HeAac => &["s16"],
            _ => &["fltp", "flt", "s32p", "s16p", "s16"],
        };
        result.sample_format = Some(choose_sample(&result.encoder, preferred)?);
    }
    if !declared.sample_rates.is_empty() && !declared.sample_rates.contains(&rate) {
        return Err(invalid(format!(
            "{} cannot encode {rate} Hz. Choose a supported sample rate.",
            result.encoder
        )));
    }
    encoders::require_audio_format(
        &result.encoder,
        result.sample_format.as_deref().unwrap(),
        rate,
        layout,
    )?;
    if !lossless && !(codec == AudioCodec::Vorbis && bitrate.is_none()) {
        result.bit_rate = Some(lossy_bitrate(codec, rate, channels, bitrate)?);
    }
    result.channel_layout = Some(layout.into());
    Ok(result)
}

/// Verify semantic pixel properties rather than packed/planar storage names.
/// Container inspection is performed after mux finalization, independently of
/// the encoder context that produced it.
pub(crate) fn verify_video_representation(
    actual: &VideoInfo,
    source: &VideoInfo,
    settings: &EncodingSettings,
) -> Result<(), EngineError> {
    let invalid = || EngineError::InvalidMedia {
        detail: "The finalized video did not preserve its accepted pixel or HDR representation."
            .into(),
    };
    let expected_pixel = pixel(settings.pixel_format.as_deref().ok_or_else(invalid)?)?;
    let actual_pixel = pixel(actual.pixel_format.as_deref().ok_or_else(invalid)?)?;
    if expected_pixel.depth != actual_pixel.depth
        || expected_pixel.rgb != actual_pixel.rgb
        || expected_pixel.alpha != actual_pixel.alpha
        || expected_pixel.floating != actual_pixel.floating
        || !expected_pixel.rgb
            && (expected_pixel.chroma_w != actual_pixel.chroma_w
                || expected_pixel.chroma_h != actual_pixel.chroma_h)
    {
        return Err(invalid());
    }
    fn close(left: &crate::Rational, right: &crate::Rational) -> bool {
        if left.denominator == 0 || right.denominator == 0 {
            return false;
        }
        let left = f64::from(left.numerator) / f64::from(left.denominator);
        let right = f64::from(right.numerator) / f64::from(right.denominator);
        (left - right).abs() <= 0.0001f64.max(left.abs() * 0.000001)
    }
    fn optional_close(left: &Option<crate::Rational>, right: &Option<crate::Rational>) -> bool {
        match (left, right) {
            (Some(left), Some(right)) => close(left, right),
            (None, _) => true,
            _ => false,
        }
    }
    if let Some(expected) = &source.mastering_display {
        let actual = actual.mastering_display.as_ref().ok_or_else(invalid)?;
        if expected.primaries.len() != actual.primaries.len()
            || expected.white_point.len() != actual.white_point.len()
            || !expected
                .primaries
                .iter()
                .zip(&actual.primaries)
                .all(|(left, right)| close(left, right))
            || !expected
                .white_point
                .iter()
                .zip(&actual.white_point)
                .all(|(left, right)| close(left, right))
            || !optional_close(&expected.min_luminance, &actual.min_luminance)
            || !optional_close(&expected.max_luminance, &actual.max_luminance)
        {
            return Err(invalid());
        }
    }
    if let Some(expected) = &source.content_light {
        let actual = actual.content_light.as_ref().ok_or_else(invalid)?;
        if expected.max_content_light_level != actual.max_content_light_level
            || expected.max_frame_average_light_level != actual.max_frame_average_light_level
        {
            return Err(invalid());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn video(pixel: &str, depth: u32) -> VideoInfo {
        VideoInfo {
            width: Some(320),
            height: Some(180),
            sample_aspect_ratio: None,
            display_aspect_ratio: None,
            average_frame_rate: None,
            nominal_frame_rate: None,
            variable_frame_rate: None,
            rotation_degrees_counter_clockwise: None,
            display_matrix: Vec::new(),
            pixel_format: Some(pixel.into()),
            bit_depth: Some(depth),
            color_primaries: Some("bt709".into()),
            color_transfer: Some("bt709".into()),
            color_space: Some("bt709".into()),
            color_range: Some("tv".into()),
            chroma_location: None,
            attached_picture: false,
            mastering_display: None,
            content_light: None,
        }
    }
    fn audio(sample: &str, depth: Option<u32>) -> AudioInfo {
        AudioInfo {
            sample_rate: Some(48000),
            channels: Some(2),
            channel_layout: Some("stereo".into()),
            sample_format: Some(sample.into()),
            bit_depth: depth,
            initial_padding: 0,
            trailing_padding: 0,
            seek_preroll: 0,
        }
    }
    fn option<'a>(settings: &'a EncodingSettings, key: &str) -> Option<&'a str> {
        settings
            .options
            .iter()
            .find(|option| option.key == key)
            .map(|option| option.value.as_str())
    }

    #[test]
    fn h264_retains_legacy_quality_without_reusing_it_for_other_codecs() {
        for quality in [18, 23, 28] {
            let settings = video_settings(VideoCodec::H264, &video("yuv420p", 8), quality).unwrap();
            assert_eq!(settings.encoder, "libx264");
            assert_eq!(option(&settings, "preset"), Some("veryfast"));
            assert_eq!(option(&settings, "crf"), Some(quality.to_string().as_str()));
            encoders::require_video_format(
                &settings.encoder,
                settings.pixel_format.as_deref().unwrap(),
            )
            .unwrap();
        }
        for codec in [
            VideoCodec::Hevc,
            VideoCodec::Av1,
            VideoCodec::Vp8,
            VideoCodec::Vp9,
            VideoCodec::Mpeg4,
            VideoCodec::Ffv1,
            VideoCodec::Mjpeg,
        ] {
            assert!(matches!(
                video_settings(codec, &video("yuv420p", 8), 18),
                Err(EngineError::Configuration { .. })
            ));
        }
    }

    #[test]
    fn linked_hevc_precision_is_honored_without_downconverting_hdr() {
        let mut source = video("yuv420p10le", 10);
        source.color_transfer = Some("smpte2084".into());
        let formats = encoders::encoder_formats("libx265").unwrap().unwrap();
        if formats
            .pixel_formats
            .iter()
            .any(|format| format == "yuv420p10le")
        {
            let settings = video_settings(VideoCodec::Hevc, &source, 23).unwrap();
            assert_eq!(settings.bit_depth, Some(10));
            assert_eq!(settings.pixel_format.as_deref(), Some("yuv420p10le"));
        } else {
            assert!(matches!(
                video_settings(VideoCodec::Hevc, &source, 23),
                Err(EngineError::UnsupportedMedia { .. })
            ));
        }
        for codec in [
            VideoCodec::H264,
            VideoCodec::Vp8,
            VideoCodec::Mpeg4,
            VideoCodec::Mjpeg,
        ] {
            assert!(matches!(
                video_settings(codec, &source, 23),
                Err(EngineError::UnsupportedMedia { .. })
            ));
        }
    }

    #[test]
    fn ffv1_preserves_declared_chroma_depth_and_alpha() {
        let source = video("yuva444p10le", 10);
        let settings = video_settings(VideoCodec::Ffv1, &source, 23).unwrap();
        assert_eq!(settings.pixel_format.as_deref(), Some("yuva444p10le"));
        assert_eq!(settings.bit_depth, Some(10));
        assert!(matches!(
            video_settings(VideoCodec::H264, &source, 23),
            Err(EngineError::UnsupportedMedia { .. })
        ));
    }

    #[test]
    fn codec_required_rgb_conversion_has_defined_output_color_metadata() {
        let mut source = video("rgb24", 8);
        source.color_space = Some("gbr".into());
        source.color_range = Some("pc".into());
        let settings = video_settings(VideoCodec::Mpeg4, &source, 23).unwrap();
        assert_eq!(settings.pixel_format.as_deref(), Some("yuv420p"));
        assert_eq!(settings.color_space.as_deref(), Some("bt709"));
        assert_eq!(settings.color_range.as_deref(), Some("tv"));
        let settings = video_settings(VideoCodec::Mjpeg, &source, 23).unwrap();
        assert!(settings
            .pixel_format
            .as_deref()
            .unwrap()
            .starts_with("yuvj"));
        assert_eq!(settings.color_range.as_deref(), Some("pc"));
    }

    #[test]
    fn pcm_preserves_original_precision_and_aiff_endianness() {
        let settings = audio_settings(
            AudioCodec::Pcm,
            OutputFormat::Aiff,
            &audio("s32", Some(24)),
            48000,
            2,
            "stereo",
            AudioPrecision::Original,
            None,
        )
        .unwrap();
        assert_eq!(settings.codec, "pcm_s24be");
        assert_eq!(settings.sample_format.as_deref(), Some("s32"));
        assert_eq!(settings.bit_depth, Some(24));
        let settings = audio_settings(
            AudioCodec::Pcm,
            OutputFormat::Wav,
            &audio("dbl", Some(64)),
            48000,
            2,
            "stereo",
            AudioPrecision::Original,
            None,
        )
        .unwrap();
        assert_eq!(settings.codec, "pcm_f64le");
        assert_eq!(settings.bit_depth, Some(64));
        let settings = audio_settings(
            AudioCodec::Pcm,
            OutputFormat::Wav,
            &audio("s64", Some(64)),
            48000,
            2,
            "stereo",
            AudioPrecision::Original,
            None,
        )
        .unwrap();
        assert_eq!(settings.codec, "pcm_s64le");
        assert_eq!(settings.sample_format.as_deref(), Some("s64"));
        assert_eq!(settings.bit_depth, Some(64));
    }

    #[test]
    fn integer_lossless_never_silently_reduces_original_precision() {
        for codec in [AudioCodec::Flac, AudioCodec::Alac] {
            assert!(matches!(
                audio_settings(
                    codec,
                    OutputFormat::Matroska,
                    &audio("s32", Some(32)),
                    48000,
                    2,
                    "stereo",
                    AudioPrecision::Original,
                    None
                ),
                Err(EngineError::Configuration { .. })
            ));
            let settings = audio_settings(
                codec,
                OutputFormat::Matroska,
                &audio("s32", Some(32)),
                48000,
                2,
                "stereo",
                AudioPrecision::Pcm24,
                None,
            )
            .unwrap();
            assert_eq!(settings.bit_depth, Some(24));
        }
        assert!(matches!(
            audio_settings(
                AudioCodec::Flac,
                OutputFormat::Flac,
                &audio("flt", Some(32)),
                48000,
                2,
                "stereo",
                AudioPrecision::Original,
                None
            ),
            Err(EngineError::Configuration { .. })
        ));
        assert!(matches!(
            audio_settings(
                AudioCodec::Aac,
                OutputFormat::M4a,
                &audio("s32", Some(24)),
                48000,
                2,
                "stereo",
                AudioPrecision::Pcm24,
                None
            ),
            Err(EngineError::Configuration { .. })
        ));
    }

    #[test]
    fn wavpack_float_and_pcm_requests_use_actual_declared_sample_formats() {
        let settings = audio_settings(
            AudioCodec::Wavpack,
            OutputFormat::Wavpack,
            &audio("flt", Some(32)),
            48000,
            2,
            "stereo",
            AudioPrecision::Original,
            None,
        )
        .unwrap();
        assert_eq!(settings.sample_format.as_deref(), Some("fltp"));
        assert_eq!(settings.bit_depth, Some(32));
        assert!(matches!(
            audio_settings(
                AudioCodec::Wavpack,
                OutputFormat::Wavpack,
                &audio("dbl", Some(64)),
                48000,
                2,
                "stereo",
                AudioPrecision::Original,
                None
            ),
            Err(EngineError::Configuration { .. })
        ));
    }

    #[test]
    fn integer_64_bit_sources_are_not_silently_narrowed() {
        for codec in [AudioCodec::Flac, AudioCodec::Alac, AudioCodec::Wavpack] {
            assert!(matches!(
                audio_settings(
                    codec,
                    OutputFormat::Matroska,
                    &audio("s64", Some(64)),
                    48000,
                    2,
                    "stereo",
                    AudioPrecision::Original,
                    None,
                ),
                Err(EngineError::Configuration { .. })
            ));
        }
        assert!(matches!(
            audio_settings(
                AudioCodec::Flac,
                OutputFormat::Flac,
                &audio("s32", None),
                48000,
                2,
                "stereo",
                AudioPrecision::Original,
                None,
            ),
            Err(EngineError::Configuration { .. })
        ));
    }

    #[test]
    fn sampling_layout_and_rate_control_conflicts_are_rejected() {
        let source = audio("s16", Some(16));
        assert!(matches!(
            audio_settings(
                AudioCodec::Mp3,
                OutputFormat::Mp3,
                &source,
                8000,
                2,
                "stereo",
                AudioPrecision::Original,
                Some(256000)
            ),
            Err(EngineError::Configuration { .. })
        ));
        assert!(matches!(
            audio_settings(
                AudioCodec::Opus,
                OutputFormat::Opus,
                &source,
                48000,
                6,
                "5.1(side)",
                AudioPrecision::Original,
                None
            ),
            Err(EngineError::Configuration { .. })
        ));
        assert!(matches!(
            audio_settings(
                AudioCodec::Flac,
                OutputFormat::Flac,
                &source,
                48000,
                2,
                "stereo",
                AudioPrecision::Original,
                Some(192000)
            ),
            Err(EngineError::Configuration { .. })
        ));
        let settings = audio_settings(
            AudioCodec::Opus,
            OutputFormat::Opus,
            &source,
            48000,
            6,
            "5.1",
            AudioPrecision::Original,
            None,
        )
        .unwrap();
        assert_eq!(option(&settings, "mapping_family"), Some("1"));
        assert_eq!(settings.sample_format.as_deref(), Some("flt"));
    }

    #[test]
    fn aac_retains_explicit_side_back_and_seven_one_layouts() {
        for (layout, channels) in [("5.1", 6), ("5.1(side)", 6), ("7.1", 8)] {
            let settings = audio_settings(
                AudioCodec::Aac,
                OutputFormat::M4a,
                &audio("flt", None),
                48000,
                channels,
                layout,
                AudioPrecision::Original,
                None,
            )
            .unwrap();
            assert_eq!(settings.channel_layout.as_deref(), Some(layout));
            assert_eq!(
                option(&settings, "aac_pce"),
                if layout == "5.1" { None } else { Some("1") }
            );
        }
    }

    #[test]
    fn missing_conditional_encoders_are_never_claimed_as_available() {
        let source = audio("s16", Some(16));
        for (codec, name, rate) in [
            (AudioCodec::HeAac, "libfdk_aac", 48000),
            (AudioCodec::AmrNb, "libopencore_amrnb", 8000),
            (AudioCodec::AmrWb, "libvo_amrwbenc", 16000),
        ] {
            let result = audio_settings(
                codec,
                OutputFormat::Amr,
                &source,
                rate,
                1,
                "mono",
                AudioPrecision::Original,
                None,
            );
            if encoders::encoder_formats(name).unwrap().is_none() {
                assert!(matches!(result, Err(EngineError::UnsupportedMedia { .. })));
            } else {
                assert_eq!(result.unwrap().encoder, name);
            }
        }
    }
}
