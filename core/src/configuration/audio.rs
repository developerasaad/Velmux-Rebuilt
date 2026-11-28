use super::{
    invalid, AudioCodec, AudioLayout, AudioPrecision, CompressionMode, ConversionConfig,
    OutputFormat,
};
use crate::{AudioInfo, EngineError, StreamInfo};

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
