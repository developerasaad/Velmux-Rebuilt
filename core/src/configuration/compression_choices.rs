//! Applicable Custom controls are projections of cached facts and the existing
//! encoder/compression authority, never sessions or per-field negotiation.
use super::{
    apply_compression,
    compression::{audio_control_bounds, audio_preset, video_control_bounds, video_preset},
    encoder_accepted, invalid, requested_audio_layout, resolve_audio_codec, resolve_audio_output,
    resolve_format, resolve_video_output, video_codec, video_encoding_settings, AudioCodec,
    AudioRateControl, CompressionMedia, CompressionMode, CompressionSettings, ConversionConfig,
    Operation, VideoCodec, VideoRateControl,
};
use crate::{processing::encoding::EncodingSettings, EngineError, MediaInfo, StreamKind};

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum CompressionControlKind {
    VideoAverageBitrate,
    H264Crf,
    HevcCrf,
    Av1Crf,
    Vp8Cq,
    Vp9Crf,
    Mpeg4Qscale,
    MjpegQscale,
    Ffv1Context,
    AudioAverageBitrate,
    Mp3VbrQuality,
    VorbisQuality,
    FlacLevel,
    AlacLevel,
    WavpackLevel,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct CompressionControlOption {
    pub kind: CompressionControlKind,
    pub label: String,
    pub unit: String,
    pub minimum: u64,
    pub maximum: u64,
    /// Unknown ABR defaults require an explicit user value, never a guessed rate.
    pub default_value: Option<u64>,
    /// Empty means a continuous integer interval; otherwise these exact values.
    pub values: Vec<u64>,
}

#[derive(Clone, Debug, Default, uniffi::Record)]
pub struct CompressionChoices {
    pub video: Vec<CompressionControlOption>,
    pub audio: Vec<CompressionControlOption>,
}

impl CompressionControlKind {
    pub(crate) fn video_control(self, value: u64) -> Result<VideoRateControl, EngineError> {
        use CompressionControlKind::*;
        let byte = || {
            u8::try_from(value)
                .map_err(|_| invalid("Enter a value within this codec's control limits."))
        };
        let control = match self {
            VideoAverageBitrate => VideoRateControl::AverageBitrate {
                bits_per_second: value,
            },
            H264Crf => VideoRateControl::H264Crf { value: byte()? },
            HevcCrf => VideoRateControl::HevcCrf { value: byte()? },
            Av1Crf => VideoRateControl::Av1Crf { value: byte()? },
            Vp8Cq => VideoRateControl::Vp8Cq { value: byte()? },
            Vp9Crf => VideoRateControl::Vp9Crf { value: byte()? },
            Mpeg4Qscale => VideoRateControl::Mpeg4Qscale { value: byte()? },
            MjpegQscale => VideoRateControl::MjpegQscale { value: byte()? },
            Ffv1Context => VideoRateControl::Ffv1Context { large: value != 0 },
            _ => return Err(invalid("Choose a video compression control.")),
        };
        let (minimum, maximum) = video_control_bounds(&control);
        if !(minimum..=maximum).contains(&value) {
            return Err(invalid("Enter a value within this codec's control limits."));
        }
        Ok(control)
    }

    pub(crate) fn audio_control(self, value: u64) -> Result<AudioRateControl, EngineError> {
        use CompressionControlKind::*;
        let byte = || {
            u8::try_from(value)
                .map_err(|_| invalid("Enter a value within this codec's control limits."))
        };
        let control = match self {
            AudioAverageBitrate if value > 0 => AudioRateControl::AverageBitrate {
                bits_per_second: u32::try_from(value)
                    .map_err(|_| invalid("The audio bitrate is too large."))?,
            },
            Mp3VbrQuality => AudioRateControl::Mp3VbrQuality { value: byte()? },
            VorbisQuality => AudioRateControl::VorbisQuality { value: byte()? },
            FlacLevel => AudioRateControl::FlacLevel { value: byte()? },
            AlacLevel => AudioRateControl::AlacLevel { value: byte()? },
            WavpackLevel => AudioRateControl::WavpackLevel { value: byte()? },
            _ => {
                return Err(invalid(
                    "Choose a positive applicable audio compression control.",
                ))
            }
        };
        if audio_control_bounds(&control)
            .is_some_and(|(minimum, maximum)| !(minimum..=maximum).contains(&value))
        {
            return Err(invalid("Enter a value within this codec's control limits."));
        }
        Ok(control)
    }

    fn presentation(self) -> (&'static str, &'static str) {
        use CompressionControlKind::*;
        match self {
            VideoAverageBitrate | AudioAverageBitrate => ("Average bitrate", "bit/s"),
            H264Crf => ("H.264 constant quality", "CRF"),
            HevcCrf => ("HEVC constant quality", "CRF"),
            Av1Crf => ("AV1 constant quality", "CRF"),
            Vp8Cq => ("VP8 constant quality", "CQ"),
            Vp9Crf => ("VP9 constant quality", "CRF"),
            Mpeg4Qscale => ("MPEG-4 quality", "QScale"),
            MjpegQscale => ("MJPEG quality", "QScale"),
            Ffv1Context => ("FFV1 context effort", "context"),
            Mp3VbrQuality => ("MP3 variable quality", "quality"),
            VorbisQuality => ("Vorbis quality", "quality"),
            FlacLevel => ("FLAC compression effort", "level"),
            AlacLevel => ("ALAC compression effort", "level"),
            WavpackLevel => ("WavPack compression effort", "level"),
        }
    }
}

fn video_kind(control: &VideoRateControl) -> (CompressionControlKind, u64) {
    use VideoRateControl::*;
    match control {
        AverageBitrate { bits_per_second } => (
            CompressionControlKind::VideoAverageBitrate,
            *bits_per_second,
        ),
        H264Crf { value } => (CompressionControlKind::H264Crf, u64::from(*value)),
        HevcCrf { value } => (CompressionControlKind::HevcCrf, u64::from(*value)),
        Av1Crf { value } => (CompressionControlKind::Av1Crf, u64::from(*value)),
        Vp8Cq { value } => (CompressionControlKind::Vp8Cq, u64::from(*value)),
        Vp9Crf { value } => (CompressionControlKind::Vp9Crf, u64::from(*value)),
        Mpeg4Qscale { value } => (CompressionControlKind::Mpeg4Qscale, u64::from(*value)),
        MjpegQscale { value } => (CompressionControlKind::MjpegQscale, u64::from(*value)),
        Ffv1Context { large } => (CompressionControlKind::Ffv1Context, u64::from(*large)),
    }
}

fn audio_kind(control: &AudioRateControl) -> (CompressionControlKind, u64) {
    use AudioRateControl::*;
    match control {
        AverageBitrate { bits_per_second } => (
            CompressionControlKind::AudioAverageBitrate,
            u64::from(*bits_per_second),
        ),
        Mp3VbrQuality { value } => (CompressionControlKind::Mp3VbrQuality, u64::from(*value)),
        VorbisQuality { value } => (CompressionControlKind::VorbisQuality, u64::from(*value)),
        FlacLevel { value } => (CompressionControlKind::FlacLevel, u64::from(*value)),
        AlacLevel { value } => (CompressionControlKind::AlacLevel, u64::from(*value)),
        WavpackLevel { value } => (CompressionControlKind::WavpackLevel, u64::from(*value)),
    }
}

fn option(
    kind: CompressionControlKind,
    bounds: (u64, u64),
    default_value: Option<u64>,
    values: Vec<u64>,
) -> CompressionControlOption {
    let (label, unit) = kind.presentation();
    CompressionControlOption {
        kind,
        label: label.into(),
        unit: unit.into(),
        minimum: bounds.0,
        maximum: bounds.1,
        default_value,
        values,
    }
}

fn prospective(config: &ConversionConfig) -> ConversionConfig {
    let mut projected = config.clone();
    projected.quality = 23;
    projected.audio_bitrate = None;
    projected.compression = Some(CompressionSettings {
        reduce_file_size: true,
        mode: CompressionMode::Custom,
        ..Default::default()
    });
    projected
}

fn supported(
    profile: &EncodingSettings,
    media: CompressionMedia,
    video: Option<VideoRateControl>,
    audio: Option<AudioRateControl>,
) -> bool {
    apply_compression(
        profile,
        media,
        &CompressionSettings {
            reduce_file_size: true,
            mode: CompressionMode::Custom,
            custom_video: video,
            custom_audio: audio,
            ..Default::default()
        },
        None,
    )
    .is_ok()
}

pub(super) fn video(info: &MediaInfo, config: &ConversionConfig) -> Vec<CompressionControlOption> {
    if config.operation != Operation::VideoConverter || config.video_codec == VideoCodec::Copy {
        return Vec::new();
    }
    let Ok(format) = resolve_format(info, config) else {
        return Vec::new();
    };
    let mut tuples = Vec::new();
    let mut source_bitrate = None;
    for stream in info
        .streams
        .iter()
        .filter(|stream| super::primary_video(stream))
    {
        let Some(facts) = &stream.video else {
            return Vec::new();
        };
        let Ok(profile) =
            video_encoding_settings(format, config.video_codec, facts, &stream.codec, 23)
        else {
            return Vec::new();
        };
        let Ok(codec) = video_codec(&profile.codec) else {
            return Vec::new();
        };
        let Ok(default) = video_preset(codec, CompressionMode::Balanced) else {
            return Vec::new();
        };
        let mut projected = prospective(config);
        projected.compression.as_mut().unwrap().custom_video = Some(default.clone());
        let Ok(Some(output)) = resolve_video_output(stream, format, &projected) else {
            return Vec::new();
        };
        if !encoder_accepted(format, &output.settings.encoder) {
            return Vec::new();
        }
        tuples.push((output.settings, CompressionMedia::Video { codec }, default));
        source_bitrate = match (source_bitrate, stream.bit_rate.filter(|rate| *rate > 0)) {
            (None, value) if tuples.len() == 1 => value,
            (Some(left), Some(right)) => Some(left.min(right)),
            _ => None,
        };
    }
    let Some((_, _, default)) = tuples.first() else {
        return Vec::new();
    };
    let mut result = Vec::new();
    let (kind, default_value) = video_kind(default);
    if tuples
        .iter()
        .all(|(profile, media, _)| supported(profile, *media, Some(default.clone()), None))
    {
        let values = if kind == CompressionControlKind::Ffv1Context {
            vec![0, 1]
        } else {
            Vec::new()
        };
        result.push(option(
            kind,
            video_control_bounds(default),
            Some(default_value),
            values,
        ));
    }
    let abr = CompressionControlKind::VideoAverageBitrate
        .video_control(1)
        .unwrap();
    if tuples
        .iter()
        .all(|(profile, media, _)| supported(profile, *media, Some(abr.clone()), None))
    {
        let bounds = video_control_bounds(&abr);
        let explicit = config
            .compression
            .as_ref()
            .and_then(|settings| settings.custom_video.as_ref())
            .and_then(|control| {
                let (kind, value) = video_kind(control);
                (kind == CompressionControlKind::VideoAverageBitrate).then_some(value)
            });
        let initial = explicit
            .or(source_bitrate)
            .filter(|value| (bounds.0..=bounds.1).contains(value));
        result.push(option(
            CompressionControlKind::VideoAverageBitrate,
            bounds,
            initial,
            Vec::new(),
        ));
    }
    result
}

pub(super) fn audio(
    info: &MediaInfo,
    config: &ConversionConfig,
    bitrates: &[u32],
) -> Vec<CompressionControlOption> {
    if config.audio_codec == AudioCodec::Copy {
        return Vec::new();
    }
    let Ok(format) = resolve_format(info, config) else {
        return Vec::new();
    };
    let mut tuples = Vec::new();
    for stream in info
        .streams
        .iter()
        .filter(|stream| stream.kind == StreamKind::Audio)
    {
        let Ok(codec) = resolve_audio_codec(format, config.audio_codec, stream) else {
            return Vec::new();
        };
        let Some(facts) = stream.audio.as_ref() else {
            return Vec::new();
        };
        let Some(rate) = config.sample_rate.or(facts.sample_rate) else {
            return Vec::new();
        };
        let Ok((channels, _)) = requested_audio_layout(facts, config) else {
            return Vec::new();
        };
        let Ok(default) = audio_preset(codec, rate, channels, CompressionMode::Balanced) else {
            return Vec::new();
        };
        let mut projected = prospective(config);
        projected.compression.as_mut().unwrap().custom_audio = Some(default.clone());
        let Ok(Some(output)) = resolve_audio_output(stream, format, &projected) else {
            return Vec::new();
        };
        if !encoder_accepted(format, &output.settings.encoder) {
            return Vec::new();
        }
        tuples.push((
            output.settings,
            CompressionMedia::Audio {
                codec,
                sample_rate: rate,
                channels,
            },
            default,
        ));
    }
    let Some((_, _, default)) = tuples.first() else {
        return Vec::new();
    };
    let mut result = Vec::new();
    if let Some(bounds) = audio_control_bounds(default) {
        if tuples
            .iter()
            .all(|(profile, media, _)| supported(profile, *media, None, Some(default.clone())))
        {
            let (kind, value) = audio_kind(default);
            result.push(option(kind, bounds, Some(value), Vec::new()));
        }
    }
    let fallback;
    let bitrates = if bitrates.is_empty()
        && tuples.iter().all(|(_, media, _)| {
            matches!(media, CompressionMedia::Audio { codec, .. } if !super::lossless_codec(*codec))
        })
    {
        // An incompatible retained Custom control must not hide the controls
        // that let the user resolve that conflict. Reuse the same candidates
        // and validators, changing only this temporary projection.
        let mut clean = prospective(config);
        clean.compression = None;
        let streams: Vec<_> = info.streams.iter().filter(|stream| stream.kind == StreamKind::Audio).collect();
        fallback = super::audio_bitrate_choices(&streams, format, &clean);
        fallback.as_slice()
    } else {
        bitrates
    };
    let mut values: Vec<_> = bitrates
        .iter()
        .copied()
        .filter(|rate| *rate > 0)
        .map(u64::from)
        .filter(|rate| {
            let Ok(control) = CompressionControlKind::AudioAverageBitrate.audio_control(*rate)
            else {
                return false;
            };
            tuples
                .iter()
                .all(|(profile, media, _)| supported(profile, *media, None, Some(control.clone())))
        })
        .collect();
    values.sort_unstable();
    values.dedup();
    if let (Some(minimum), Some(maximum)) = (values.first(), values.last()) {
        let desired = tuples
            .iter()
            .filter_map(|(_, _, default)| match default {
                AudioRateControl::AverageBitrate { bits_per_second } => {
                    Some(u64::from(*bits_per_second))
                }
                _ => None,
            })
            .min()
            .unwrap_or(*minimum);
        let default = values
            .iter()
            .copied()
            .min_by_key(|rate| (rate.abs_diff(desired), *rate));
        result.push(option(
            CompressionControlKind::AudioAverageBitrate,
            (*minimum, *maximum),
            default,
            values,
        ));
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    fn video_stream(hdr: bool) -> Value {
        json!({"id":"cached:0:0","index":0,"container_id":0,"kind":"Video",
            "codec_id":if hdr {crate::ffmpeg::sys::AV_CODEC_ID_HEVC} else {crate::ffmpeg::sys::AV_CODEC_ID_H264},
            "codec":if hdr {"hevc"} else {"h264"},"decoder_available":true,
            "duration_us":2000000,"start_time_us":0,"disposition":0,"metadata":[],
            "video":{"width":320,"height":180,"display_matrix":[],"attached_picture":false,
                "pixel_format":if hdr {"yuv420p10le"} else {"yuv420p"},"bit_depth":if hdr {10} else {8},
                "color_transfer":if hdr {"smpte2084"} else {"bt709"},
                "color_space":if hdr {"bt2020nc"} else {"bt709"},
                "nominal_frame_rate":{"numerator":24,"denominator":1}}})
    }

    fn audio_stream(index: u32, codec: &str, rate: u32) -> Value {
        json!({"id":format!("cached:{index}:{index}"),"index":index,"container_id":index,"kind":"Audio",
            "codec_id":match codec {"aac"=>crate::ffmpeg::sys::AV_CODEC_ID_AAC,
                "mp3"=>crate::ffmpeg::sys::AV_CODEC_ID_MP3,_=>crate::ffmpeg::sys::AV_CODEC_ID_PCM_S16LE},
            "codec":codec,"decoder_available":true,"duration_us":2000000,"start_time_us":0,
            "disposition":0,"profile":if codec=="aac" {Some("LC")} else {None},"metadata":[],
            "audio":{"sample_rate":rate,"channels":2,"channel_layout":"stereo",
                "sample_format":"s16","bit_depth":16,"initial_padding":0,"trailing_padding":0,"seek_preroll":0}})
    }

    fn media(streams: Vec<Value>) -> MediaInfo {
        serde_json::from_value(json!({"source_identity":"cached","byte_size":1000000,
            "container":"matroska","duration_us":2000000,"streams":streams,"metadata":[],
            "chapters":[],"staged":false}))
        .unwrap()
    }

    fn config() -> ConversionConfig {
        serde_json::from_value(json!({"operation":"VideoConverter","format":"Matroska",
            "video_codec":"Automatic","audio_codec":"Automatic","video_size":"Original",
            "frame_rate":"Original","quality":23,"file_name":"video"}))
        .unwrap()
    }

    #[test]
    fn automatic_and_hdr_controls_follow_actual_encoder_representation() {
        for (hdr, expected) in [
            (false, CompressionControlKind::H264Crf),
            (true, CompressionControlKind::HevcCrf),
        ] {
            let source = media(vec![video_stream(hdr)]);
            let config = config();
            let original = serde_json::to_value(&config).unwrap();
            let choices = video(&source, &config);
            assert!(choices
                .iter()
                .any(|option| option.kind == expected && option.default_value == Some(23)));
            let abr = choices
                .iter()
                .find(|option| option.kind == CompressionControlKind::VideoAverageBitrate)
                .unwrap();
            assert!(
                abr.default_value.is_none(),
                "A file's aggregate bitrate is not a video bitrate default"
            );
            assert_eq!(serde_json::to_value(&config).unwrap(), original);
        }
        let mut copy = config();
        copy.video_codec = VideoCodec::Copy;
        assert!(video(&media(vec![video_stream(false)]), &copy).is_empty());
    }

    #[test]
    fn audio_controls_intersect_every_retained_codec_and_sampling_tuple() {
        let source = media(vec![
            audio_stream(0, "mp3", 8000),
            audio_stream(1, "mp3", 48000),
        ]);
        let choices = audio(
            &source,
            &config(),
            &[8000, 32000, 64000, 96000, 128000, 320000],
        );
        assert!(choices
            .iter()
            .any(|option| option.kind == CompressionControlKind::Mp3VbrQuality));
        let abr = choices
            .iter()
            .find(|option| option.kind == CompressionControlKind::AudioAverageBitrate)
            .unwrap();
        assert_eq!(abr.values, vec![32000, 64000]);
        assert!(abr.values.contains(&abr.default_value.unwrap()));
        let mixed = media(vec![
            audio_stream(0, "aac", 48000),
            audio_stream(1, "mp3", 8000),
        ]);
        let mixed_choices = audio(&mixed, &config(), &[8000, 32000, 64000, 128000]);
        assert_eq!(mixed_choices.len(), 1);
        assert_eq!(
            mixed_choices[0].kind,
            CompressionControlKind::AudioAverageBitrate
        );
        assert_eq!(mixed_choices[0].values, vec![8000, 32000, 64000]);
        let mut copy = config();
        copy.audio_codec = AudioCodec::Copy;
        assert!(audio(&mixed, &copy, &[32000]).is_empty());
        assert!(audio(
            &media(vec![audio_stream(0, "pcm_s16le", 48000)]),
            &config(),
            &[32000]
        )
        .is_empty());
    }

    #[test]
    fn every_published_default_constructs_a_valid_codec_specific_control() {
        let source = media(vec![video_stream(false), audio_stream(1, "aac", 48000)]);
        for codec in [
            VideoCodec::H264,
            VideoCodec::Hevc,
            VideoCodec::Av1,
            VideoCodec::Vp8,
            VideoCodec::Vp9,
            VideoCodec::Mpeg4,
            VideoCodec::Mjpeg,
            VideoCodec::Ffv1,
        ] {
            let mut config = config();
            config.video_codec = codec;
            let options = video(&source, &config);
            assert!(!options.is_empty(), "No actual controls for {codec:?}");
            for option in options {
                if let Some(value) = option.default_value {
                    let control = option.kind.video_control(value).unwrap();
                    let mut projected = prospective(&config);
                    projected.compression.as_mut().unwrap().custom_video = Some(control.clone());
                    let format = resolve_format(&source, &config).unwrap();
                    let output = resolve_video_output(&source.streams[0], format, &projected)
                        .unwrap()
                        .unwrap();
                    assert!(supported(
                        &output.settings,
                        CompressionMedia::Video { codec },
                        Some(control),
                        None
                    ));
                }
            }
        }
        for codec in [
            AudioCodec::Aac,
            AudioCodec::Mp3,
            AudioCodec::Vorbis,
            AudioCodec::Flac,
            AudioCodec::Alac,
            AudioCodec::Wavpack,
        ] {
            let mut config = config();
            config.audio_codec = codec;
            let options = audio(&source, &config, &[32000, 64000, 128000, 256000]);
            assert!(!options.is_empty(), "No actual controls for {codec:?}");
            for option in options {
                let value = option.default_value.unwrap();
                let control = option.kind.audio_control(value).unwrap();
                let mut projected = prospective(&config);
                projected.compression.as_mut().unwrap().custom_audio = Some(control.clone());
                let format = resolve_format(&source, &config).unwrap();
                let output = resolve_audio_output(&source.streams[1], format, &projected)
                    .unwrap()
                    .unwrap();
                assert!(supported(
                    &output.settings,
                    CompressionMedia::Audio {
                        codec,
                        sample_rate: 48000,
                        channels: 2
                    },
                    None,
                    Some(control)
                ));
            }
        }
        assert!(CompressionControlKind::Ffv1Context
            .video_control(2)
            .is_err());
        assert!(CompressionControlKind::H264Crf.video_control(52).is_err());
        assert!(CompressionControlKind::AlacLevel.audio_control(0).is_err());
        assert!(CompressionControlKind::AudioAverageBitrate
            .audio_control(0)
            .is_err());
    }

    #[test]
    fn conflicting_custom_intent_retains_applicable_replacement_controls() {
        let source = media(vec![video_stream(false), audio_stream(1, "aac", 48000)]);
        let mut config = config();
        config.audio_codec = AudioCodec::Aac;
        config.compression = Some(CompressionSettings {
            reduce_file_size: true,
            mode: CompressionMode::Custom,
            custom_audio: Some(AudioRateControl::FlacLevel { value: 5 }),
            ..Default::default()
        });
        let original = serde_json::to_value(&config).unwrap();
        let choices = super::super::choices(&source, &config);
        assert!(
            choices.audio_bitrates.is_empty(),
            "Ordinary selectors still report the explicit conflict"
        );
        let abr = choices
            .compression_choices
            .audio
            .iter()
            .find(|option| option.kind == CompressionControlKind::AudioAverageBitrate)
            .expect("A conflicting FLAC control must not hide replacement AAC controls");
        assert!(abr.values.contains(&128000));
        assert!(abr.values.contains(&abr.default_value.unwrap()));
        assert_eq!(serde_json::to_value(&config).unwrap(), original);
        let mut accepted_alternative = config.clone();
        accepted_alternative
            .compression
            .as_mut()
            .unwrap()
            .custom_audio = Some(abr.kind.audio_control(128000).unwrap());
        let output = resolve_audio_output(
            &source.streams[1],
            super::super::OutputFormat::Matroska,
            &accepted_alternative,
        )
        .unwrap()
        .unwrap();
        assert!(supported(
            &output.settings,
            CompressionMedia::Audio {
                codec: AudioCodec::Aac,
                sample_rate: 48000,
                channels: 2
            },
            None,
            accepted_alternative.compression.unwrap().custom_audio
        ));
    }
}
