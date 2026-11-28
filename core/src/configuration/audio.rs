use super::{
    invalid, AudioCodec, AudioLayout, AudioPrecision, CompressionMode, ConversionConfig,
    OutputFormat,
};
use crate::{
    processing::encoding::{self, EncodingSettings},
    AudioInfo, EngineError, StreamInfo,
};

pub(crate) const SAMPLE_RATES: [u32; 15] = [
    0, 8000, 11025, 12000, 16000, 22050, 24000, 32000, 44100, 48000, 64000, 88200, 96000, 176400,
    192000,
];
pub(crate) const AUDIO_LAYOUTS: [AudioLayout; 6] = [
    AudioLayout::Original,
    AudioLayout::Mono,
    AudioLayout::Stereo,
    AudioLayout::Surround51,
    AudioLayout::Surround51Side,
    AudioLayout::Surround71,
];
pub(crate) const AUDIO_PRECISIONS: [AudioPrecision; 7] = [
    AudioPrecision::Original,
    AudioPrecision::Pcm8,
    AudioPrecision::Pcm16,
    AudioPrecision::Pcm24,
    AudioPrecision::Pcm32,
    AudioPrecision::Float32,
    AudioPrecision::Float64,
];

pub(crate) fn audio_copy_allowed(format: OutputFormat, codec: &str) -> bool {
    match format {
        OutputFormat::M4a | OutputFormat::M4v | OutputFormat::Mp4 => {
            matches!(codec, "aac" | "alac" | "mp3" | "ac3" | "eac3")
        }
        OutputFormat::Wav | OutputFormat::Aiff => codec.starts_with("pcm_"),
        OutputFormat::Flac => codec == "flac",
        OutputFormat::Mp3 => codec == "mp3",
        OutputFormat::Ogg => matches!(codec, "vorbis" | "opus" | "flac"),
        OutputFormat::Opus => codec == "opus",
        OutputFormat::Aac => codec == "aac",
        OutputFormat::Amr => matches!(codec, "amr_nb" | "amr_wb"),
        OutputFormat::Wma => matches!(codec, "wmav1" | "wmav2" | "wmapro" | "wmalossless"),
        OutputFormat::Wavpack => codec == "wavpack",
        OutputFormat::Webm => matches!(codec, "vorbis" | "opus"),
        OutputFormat::Flv => matches!(
            codec,
            "aac" | "mp3" | "speex" | "nellymoser" | "adpcm_swf" | "pcm_u8" | "pcm_s16le"
        ),
        OutputFormat::ThreeGp => matches!(codec, "aac" | "amr_nb" | "amr_wb"),
        OutputFormat::Original => false,
        _ => true,
    }
}

pub(crate) fn resolve_audio_codec(
    format: OutputFormat,
    requested: AudioCodec,
    stream: &StreamInfo,
) -> Result<AudioCodec, EngineError> {
    if !matches!(requested, AudioCodec::Automatic | AudioCodec::Copy) {
        return Ok(requested);
    }
    let definition = format.definition();
    for codec in definition.audio_codecs {
        let same = if *codec == AudioCodec::Pcm {
            stream.codec.starts_with("pcm_")
        } else {
            codec.codec_name() == Some(stream.codec.as_str())
        };
        if same
            && codec
                .encoder()
                .is_some_and(|name| super::encoder_accepted(format, name))
        {
            return Ok(*codec);
        }
    }
    definition.audio_codecs.iter().copied().find(|codec| codec.encoder().is_some_and(|name| super::encoder_accepted(format, name)))
        .ok_or_else(|| invalid("No compatible audio encoder is available for this representation. Preserve a compatible source stream or choose another format."))
}

pub(crate) fn requested_audio_layout(
    source: &AudioInfo,
    config: &ConversionConfig,
) -> Result<(u32, String), EngineError> {
    if let Some(layout) = config
        .channel_layout
        .filter(|layout| *layout != AudioLayout::Original)
    {
        return Ok((layout.channels().unwrap(), layout.name().unwrap().into()));
    }
    if let Some(channels) = config.channels {
        let layout = match channels {
            1 => "mono",
            2 => "stereo",
            6 => "5.1",
            8 => "7.1",
            _ => return Err(invalid("Choose an applicable speaker layout.")),
        };
        return Ok((channels, layout.into()));
    }
    let channels = source.channels.ok_or_else(|| {
        invalid("The source channel count is unknown. Choose an explicit supported layout.")
    })?;
    let layout = source
        .channel_layout
        .as_deref()
        .filter(|name| !name.ends_with(" channels"));
    if let Some(layout) = layout {
        return Ok((channels, layout.into()));
    }
    match channels { 1=>Ok((1,"mono".into())),2=>Ok((2,"stereo".into())),
        _=>Err(invalid("This multichannel source has no declared speaker positions. Choose a source with an identified layout instead of guessing channel meaning.")) }
}

pub(crate) fn audio_transform_requested(stream: &StreamInfo, config: &ConversionConfig) -> bool {
    let Some(source) = &stream.audio else {
        return false;
    };
    let layout_change = if config.channels.is_some()
        || config
            .channel_layout
            .is_some_and(|layout| layout != AudioLayout::Original)
    {
        requested_audio_layout(source, config).map_or(true, |(channels, layout)| {
            Some(channels) != source.channels
                || source
                    .channel_layout
                    .as_deref()
                    .filter(|name| !name.ends_with(" channels"))
                    .is_some_and(|name| name != layout)
        })
    } else {
        false
    };
    let precision_change = match config.audio_precision.unwrap_or(AudioPrecision::Original) {
        AudioPrecision::Original => false,
        AudioPrecision::Pcm8 => source.bit_depth != Some(8),
        AudioPrecision::Pcm16 => source.bit_depth != Some(16),
        AudioPrecision::Pcm24 => source.bit_depth != Some(24),
        AudioPrecision::Pcm32 => {
            source.bit_depth != Some(32)
                || source
                    .sample_format
                    .as_deref()
                    .is_some_and(|name| name.starts_with("flt") || name.starts_with("dbl"))
        }
        AudioPrecision::Float32 => source
            .sample_format
            .as_deref()
            .is_none_or(|name| !name.starts_with("flt")),
        AudioPrecision::Float64 => source
            .sample_format
            .as_deref()
            .is_none_or(|name| !name.starts_with("dbl")),
    };
    config
        .sample_rate
        .is_some_and(|rate| Some(rate) != source.sample_rate)
        || layout_change
        || precision_change
        || config.audio_bitrate.is_some()
        || config
            .audio_adjustments
            .as_ref()
            .is_some_and(|settings| settings.active())
}

pub(crate) fn audio_encoding_required(
    stream: &StreamInfo,
    config: &ConversionConfig,
    compatible: bool,
) -> bool {
    let transform = audio_transform_requested(stream, config);
    if config.audio_codec == AudioCodec::Copy {
        return false;
    }
    let compression = config.compression.as_ref().is_some_and(|settings| {
        if !settings.reduce_file_size {
            return false;
        }
        if settings.mode == CompressionMode::Custom {
            return settings.custom_audio.is_some();
        }
        let lossless = if config.audio_codec == AudioCodec::Automatic {
            stream.codec.starts_with("pcm_")
                || matches!(stream.codec.as_str(), "flac" | "alac" | "wavpack")
        } else {
            matches!(
                config.audio_codec,
                AudioCodec::Pcm
                    | AudioCodec::PcmS16le
                    | AudioCodec::Flac
                    | AudioCodec::Alac
                    | AudioCodec::Wavpack
            )
        };
        !lossless
    });
    if !compatible || transform || compression {
        return true;
    }
    if config.audio_codec == AudioCodec::Automatic {
        return false;
    }
    let same = if config.audio_codec == AudioCodec::Pcm {
        stream.codec.starts_with("pcm_")
    } else {
        config.audio_codec.codec_name() == Some(stream.codec.as_str())
    };
    !same || config.audio_codec == AudioCodec::Aac && stream.profile.as_deref() != Some("LC")
}

pub(crate) fn automatic_audio_bitrate(target: &str, rate: u32, channels: u32) -> u32 {
    match target {
        "aac" => 192000.min(rate.saturating_mul(channels).saturating_mul(6)),
        "mp3" if rate < 16000 => 64000,
        "mp3" if rate < 32000 => 128000,
        "mp3" => 192000,
        "opus" => match channels {
            1 => 96000,
            2 => 160000,
            _ => 384000,
        },
        "vorbis" => match channels {
            1 => 96000,
            2 => 192000,
            _ => 384000,
        },
        "ac3" | "eac3" => {
            if channels > 2 {
                448000
            } else {
                192000
            }
        }
        "wmav1" | "wmav2" => 192000,
        "amr_nb" => 12200,
        "amr_wb" => 23850,
        _ => 0,
    }
}

/// A cheap, owned audio representation decision. Encoding profiles describe
/// the exact software tuple before composition-wide compression/target budgets.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AudioOutput {
    pub sample_rate: u32,
    pub channels: u32,
    pub settings: EncodingSettings,
}

fn validate_audio_copy(stream: &StreamInfo, config: &ConversionConfig) -> Result<(), EngineError> {
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

/// Resolve the same audio tuple for processing and lightweight configuration
/// projections. None preserves the original compatible stream. No input is
/// reopened, decoder/encoder session started, or plan
/// constructed. The caller retains final whole-composition muxer validation.
pub(crate) fn resolve_audio_output(
    stream: &StreamInfo,
    format: OutputFormat,
    config: &ConversionConfig,
) -> Result<Option<AudioOutput>, EngineError> {
    validate_audio_copy(stream, config)?;
    let audio = stream
        .audio
        .as_ref()
        .ok_or_else(|| invalid("Audio properties could not be inspected."))?;
    let compatible =
        super::muxer_accepts(format, stream.codec_id) && audio_copy_allowed(format, &stream.codec);
    if config.audio_codec == AudioCodec::Copy && audio_transform_requested(stream, config) {
        return Err(invalid(
            "These audio settings require encoding. Choose an applicable codec or Automatic.",
        ));
    }
    if config
        .audio_precision
        .is_some_and(|precision| precision != AudioPrecision::Original)
    {
        let target = if config.audio_codec == AudioCodec::Automatic && compatible {
            stream.codec.as_str()
        } else {
            resolve_audio_codec(format, config.audio_codec, stream)?
                .codec_name()
                .unwrap_or("")
        };
        if !target.starts_with("pcm_") && !matches!(target, "flac" | "alac" | "wavpack") {
            return Err(invalid("Sample precision applies to PCM and supported lossless encoders. Reset precision or choose an applicable codec."));
        }
    }
    if !audio_encoding_required(stream, config, compatible) {
        return Ok(None);
    }
    if !stream.decoder_available {
        return Err(invalid(
            "This source requires an unavailable audio decoder.",
        ));
    }
    let codec = resolve_audio_codec(format, config.audio_codec, stream)?;
    let sample_rate = config
        .sample_rate
        .or(audio.sample_rate)
        .ok_or_else(|| invalid("The source sample rate is unknown."))?;
    let (channels, layout) = requested_audio_layout(audio, config)?;
    let settings = encoding::audio_settings(
        codec,
        format,
        audio,
        sample_rate,
        channels,
        &layout,
        config.audio_precision.unwrap_or(AudioPrecision::Original),
        config.audio_bitrate,
    )?;
    Ok(Some(AudioOutput {
        sample_rate,
        channels,
        settings,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AudioRateControl, CompressionSettings};
    use serde_json::json;

    fn pcm() -> StreamInfo {
        serde_json::from_value(json!({
            "id":"source:0:0","index":0,"container_id":0,"kind":"Audio",
            "codec_id":crate::ffmpeg::sys::AV_CODEC_ID_PCM_S16LE,"codec":"pcm_s16le",
            "codec_long_name":null,"decoder_available":true,"time_base":null,
            "duration_us":2000000,"start_time_us":0,"frame_count":null,"bit_rate":1536000,
            "disposition":0,"profile":null,"level":null,"metadata":[],"video":null,
            "audio":{"sample_rate":48000,"channels":2,"channel_layout":"stereo",
                "sample_format":"s16","bit_depth":16,"initial_padding":0,
                "trailing_padding":0,"seek_preroll":0},"subtitle":null
        }))
        .unwrap()
    }

    fn config() -> ConversionConfig {
        serde_json::from_value(json!({
            "operation":"AudioConverter","format":"Aiff","video_codec":"Automatic",
            "audio_codec":"Pcm","video_size":"Original","frame_rate":"Original",
            "quality":23,"audio_bitrate":null,"sample_rate":null,"channels":null,
            "file_name":"audio"
        }))
        .unwrap()
    }

    #[test]
    fn generic_pcm_projection_preserves_compatible_sowt_without_a_decoder() {
        let mut source = pcm();
        source.decoder_available = false;
        assert_eq!(
            resolve_audio_output(&source, OutputFormat::Aiff, &config()).unwrap(),
            None
        );
        let mut transformed = config();
        transformed.sample_rate = Some(96000);
        assert!(matches!(
            resolve_audio_output(&source, OutputFormat::Aiff, &transformed),
            Err(EngineError::Configuration { .. })
        ));
    }

    #[test]
    fn transformed_pcm_projection_freezes_the_actual_sample_representation() {
        let mut config = config();
        config.sample_rate = Some(96000);
        config.channels = Some(1);
        config.audio_precision = Some(AudioPrecision::Pcm24);
        let Some(AudioOutput {
            sample_rate,
            channels,
            settings,
        }) = resolve_audio_output(&pcm(), OutputFormat::Aiff, &config).unwrap()
        else {
            panic!("A changed PCM tuple requires encoding");
        };
        assert_eq!((sample_rate, channels), (96000, 1));
        assert_eq!(settings.encoder, "pcm_s24be");
        assert_eq!(settings.codec, "pcm_s24be");
        assert_eq!(settings.bit_depth, Some(24));
        assert_eq!(settings.sample_format.as_deref(), Some("s32"));
        assert_eq!(settings.channel_layout.as_deref(), Some("mono"));
        assert!(settings.bit_rate.is_none());
    }

    #[test]
    fn copied_audio_rejects_active_rate_control_and_sample_transformations() {
        let source = pcm();
        let mut config = config();
        config.audio_codec = AudioCodec::Copy;
        config.compression = Some(CompressionSettings {
            reduce_file_size: true,
            mode: CompressionMode::Custom,
            custom_audio: Some(AudioRateControl::AverageBitrate {
                bits_per_second: 128000,
            }),
            ..Default::default()
        });
        assert!(matches!(
            resolve_audio_output(&source, OutputFormat::Aiff, &config),
            Err(EngineError::Configuration { .. })
        ));
        config.compression = None;
        config.channels = Some(1);
        assert!(matches!(
            resolve_audio_output(&source, OutputFormat::Aiff, &config),
            Err(EngineError::Configuration { .. })
        ));
    }
}
