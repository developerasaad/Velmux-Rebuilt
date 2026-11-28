use super::{invalid, Operation};
use crate::{EngineError, MediaInfo};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, uniffi::Enum)]
pub enum VideoCodec {
    Automatic,
    H264,
    Hevc,
    Av1,
    Vp8,
    Vp9,
    Mpeg4,
    Ffv1,
    Mjpeg,
    Copy,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, uniffi::Enum)]
pub enum AudioCodec {
    Automatic,
    Aac,
    Mp3,
    PcmS16le,
    Pcm,
    Flac,
    Alac,
    Opus,
    Vorbis,
    Ac3,
    Eac3,
    Wavpack,
    Wma,
    HeAac,
    AmrNb,
    AmrWb,
    Copy,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, uniffi::Enum)]
pub enum AudioPrecision {
    Original,
    Pcm8,
    Pcm16,
    Pcm24,
    Pcm32,
    Float32,
    Float64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, uniffi::Enum)]
pub enum AudioLayout {
    Original,
    Mono,
    Stereo,
    Surround51,
    Surround51Side,
    Surround71,
}
impl AudioLayout {
    pub(crate) fn channels(self) -> Option<u32> {
        match self {
            Self::Original => None,
            Self::Mono => Some(1),
            Self::Stereo => Some(2),
            Self::Surround51 | Self::Surround51Side => Some(6),
            Self::Surround71 => Some(8),
        }
    }
    pub(crate) fn name(self) -> Option<&'static str> {
        match self {
            Self::Original => None,
            Self::Mono => Some("mono"),
            Self::Stereo => Some("stereo"),
            Self::Surround51 => Some("5.1"),
            Self::Surround51Side => Some("5.1(side)"),
            Self::Surround71 => Some("7.1"),
        }
    }
}

impl VideoCodec {
    pub(crate) fn encoder(self) -> Option<&'static str> {
        match self {
            Self::H264 => Some("libx264"),
            Self::Hevc => Some("libx265"),
            Self::Av1 => Some("libsvtav1"),
            Self::Vp8 => Some("libvpx"),
            Self::Vp9 => Some("libvpx-vp9"),
            Self::Mpeg4 => Some("mpeg4"),
            Self::Ffv1 => Some("ffv1"),
            Self::Mjpeg => Some("mjpeg"),
            _ => None,
        }
    }
    pub(crate) fn codec_name(self) -> Option<&'static str> {
        match self {
            Self::H264 => Some("h264"),
            Self::Hevc => Some("hevc"),
            Self::Av1 => Some("av1"),
            Self::Vp8 => Some("vp8"),
            Self::Vp9 => Some("vp9"),
            Self::Mpeg4 => Some("mpeg4"),
            Self::Ffv1 => Some("ffv1"),
            Self::Mjpeg => Some("mjpeg"),
            _ => None,
        }
    }
}
impl AudioCodec {
    pub(crate) fn encoder(self) -> Option<&'static str> {
        match self {
            Self::Aac => Some("aac"),
            Self::Mp3 => Some("libmp3lame"),
            Self::PcmS16le => Some("pcm_s16le"),
            Self::Pcm => Some("pcm_s16le"),
            Self::Flac => Some("flac"),
            Self::Alac => Some("alac"),
            Self::Opus => Some("libopus"),
            Self::Vorbis => Some("libvorbis"),
            Self::Ac3 => Some("ac3"),
            Self::Eac3 => Some("eac3"),
            Self::Wavpack => Some("wavpack"),
            Self::Wma => Some("wmav2"),
            Self::HeAac => Some("libfdk_aac"),
            Self::AmrNb => Some("libopencore_amrnb"),
            Self::AmrWb => Some("libvo_amrwbenc"),
            _ => None,
        }
    }
    pub(crate) fn codec_name(self) -> Option<&'static str> {
        match self {
            Self::Aac | Self::HeAac => Some("aac"),
            Self::Mp3 => Some("mp3"),
            Self::PcmS16le | Self::Pcm => Some("pcm_s16le"),
            Self::Flac => Some("flac"),
            Self::Alac => Some("alac"),
            Self::Opus => Some("opus"),
            Self::Vorbis => Some("vorbis"),
            Self::Ac3 => Some("ac3"),
            Self::Eac3 => Some("eac3"),
            Self::Wavpack => Some("wavpack"),
            Self::Wma => Some("wmav2"),
            Self::AmrNb => Some("amr_nb"),
            Self::AmrWb => Some("amr_wb"),
            _ => None,
        }
    }
}

pub(crate) struct FormatDefinition {
    pub label: &'static str,
    pub muxer: &'static str,
    pub extension: &'static str,
    pub mime_type: &'static str,
    pub operation: Option<Operation>,
    pub video_codecs: &'static [VideoCodec],
    pub audio_codecs: &'static [AudioCodec],
}

macro_rules! formats {
    ($($name:ident => ($label:literal,$muxer:literal,$extension:literal,$mime:literal,$operation:expr,[$($video:ident),*],[$($audio:ident),*]));* $(;)?) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, uniffi::Enum)]
        pub enum OutputFormat { $($name),* }
        impl OutputFormat {
            pub(crate) fn definition(self) -> FormatDefinition {
                match self { $( Self::$name => FormatDefinition {
                    label:$label,muxer:$muxer,extension:$extension,mime_type:$mime,operation:$operation,
                    video_codecs:&[$(VideoCodec::$video),*],audio_codecs:&[$(AudioCodec::$audio),*],
                }),* }
            }
            pub(crate) fn muxer(self) -> &'static str { self.definition().muxer }
            pub fn extension(self) -> String { self.definition().extension.into() }
            pub fn mime_type(self) -> String { self.definition().mime_type.into() }
            pub(crate) fn is_video(self) -> bool { self.definition().operation == Some(Operation::VideoConverter) }
        }
    }
}

formats! {
    Original => ("Original","","","application/octet-stream",None,[],[]);
    Mp4 => ("MP4","mp4","mp4","video/mp4",Some(Operation::VideoConverter),[H264,Hevc,Av1,Mpeg4,Mjpeg],[Aac,Alac,Ac3,Eac3,Mp3]);
    Matroska => ("Matroska","matroska","mkv","video/x-matroska",Some(Operation::VideoConverter),[H264,Hevc,Av1,Vp8,Vp9,Mpeg4,Ffv1,Mjpeg],[Aac,Mp3,Opus,Vorbis,Flac,Alac,Pcm,Ac3,Eac3,Wavpack]);
    Mov => ("MOV","mov","mov","video/quicktime",Some(Operation::VideoConverter),[H264,Hevc,Mpeg4,Mjpeg],[Aac,Alac,Pcm,Ac3,Eac3,Mp3]);
    M4v => ("M4V","mp4","m4v","video/x-m4v",Some(Operation::VideoConverter),[H264,Hevc,Av1,Mpeg4],[Aac,Alac,Ac3,Eac3]);
    Webm => ("WebM","webm","webm","video/webm",Some(Operation::VideoConverter),[Vp9,Vp8,Av1],[Opus,Vorbis]);
    Avi => ("AVI","avi","avi","video/x-msvideo",Some(Operation::VideoConverter),[Mpeg4,Mjpeg,Ffv1],[Mp3,Pcm,Ac3]);
    MpegTs => ("MPEG-TS / MTS","mpegts","ts","video/mp2t",Some(Operation::VideoConverter),[H264,Hevc,Mpeg4],[Aac,Mp3,Ac3,Eac3]);
    Flv => ("FLV","flv","flv","video/x-flv",Some(Operation::VideoConverter),[H264],[Aac,Mp3]);
    ThreeGp => ("3GP","3gp","3gp","video/3gpp",Some(Operation::VideoConverter),[H264,Mpeg4],[Aac,AmrNb,AmrWb]);
    Asf => ("ASF","asf","asf","video/x-ms-asf",Some(Operation::VideoConverter),[Mpeg4,Mjpeg],[Wma,Mp3,Pcm]);
    M4a => ("M4A","mp4","m4a","audio/mp4",Some(Operation::AudioConverter),[],[Aac,Alac,Ac3,Eac3]);
    Mp3 => ("MP3","mp3","mp3","audio/mpeg",Some(Operation::AudioConverter),[],[Mp3]);
    Wav => ("WAV","wav","wav","audio/wav",Some(Operation::AudioConverter),[],[Pcm]);
    Flac => ("FLAC","flac","flac","audio/flac",Some(Operation::AudioConverter),[],[Flac]);
    Ogg => ("Ogg","ogg","ogg","audio/ogg",Some(Operation::AudioConverter),[],[Vorbis,Opus,Flac]);
    Opus => ("Opus","opus","opus","audio/ogg",Some(Operation::AudioConverter),[],[Opus]);
    Aac => ("AAC","adts","aac","audio/aac",Some(Operation::AudioConverter),[],[Aac,HeAac]);
    Aiff => ("AIFF","aiff","aiff","audio/aiff",Some(Operation::AudioConverter),[],[Pcm]);
    Amr => ("AMR","amr","amr","audio/amr",Some(Operation::AudioConverter),[],[AmrNb,AmrWb]);
    Wma => ("WMA / ASF","asf","wma","audio/x-ms-wma",Some(Operation::AudioConverter),[],[Wma]);
    Wavpack => ("WavPack","wv","wv","audio/wavpack",Some(Operation::AudioConverter),[],[Wavpack]);
}

pub(crate) fn product_formats(operation: Operation) -> Vec<OutputFormat> {
    if operation == Operation::VideoConverter {
        vec![
            OutputFormat::Original,
            OutputFormat::Mp4,
            OutputFormat::Matroska,
            OutputFormat::Mov,
            OutputFormat::M4v,
            OutputFormat::Webm,
            OutputFormat::Avi,
            OutputFormat::MpegTs,
            OutputFormat::Flv,
            OutputFormat::ThreeGp,
            OutputFormat::Asf,
        ]
    } else {
        vec![
            OutputFormat::Original,
            OutputFormat::Mp3,
            OutputFormat::M4a,
            OutputFormat::Wav,
            OutputFormat::Flac,
            OutputFormat::Ogg,
            OutputFormat::Opus,
            OutputFormat::Aac,
            OutputFormat::Aiff,
            OutputFormat::Amr,
            OutputFormat::Wma,
            OutputFormat::Wavpack,
        ]
    }
}

/// Resolve only facts known from actual inspection. The common MOV-family
/// demuxer name alone does not distinguish MP4, MOV, M4A, M4V or 3GP.
pub(crate) fn original_format(source: &MediaInfo, operation: Operation) -> Option<OutputFormat> {
    let names: Vec<_> = source.container.split(',').collect();
    let brand = source
        .metadata
        .iter()
        .find(|entry| entry.key == "major_brand")
        .map(|entry| entry.value.as_str());
    if names.contains(&"mov") || names.contains(&"mp4") {
        return match brand.map(str::trim) {
            Some("qt") => Some(OutputFormat::Mov),
            Some("M4V" | "M4VH" | "M4VP") => Some(OutputFormat::M4v),
            Some("M4A" | "M4B") if operation == Operation::AudioConverter => {
                Some(OutputFormat::M4a)
            }
            Some(value) if value.starts_with("3gp") || value.starts_with("3g2") => {
                Some(OutputFormat::ThreeGp)
            }
            Some(
                "isom" | "iso2" | "iso3" | "iso4" | "iso5" | "iso6" | "mp41" | "mp42" | "avc1"
                | "dash",
            ) => Some(if operation == Operation::AudioConverter {
                OutputFormat::M4a
            } else {
                OutputFormat::Mp4
            }),
            _ => None,
        };
    }
    for (name, format) in [
        ("wav", OutputFormat::Wav),
        ("flac", OutputFormat::Flac),
        ("mp3", OutputFormat::Mp3),
        ("aac", OutputFormat::Aac),
        ("aiff", OutputFormat::Aiff),
        ("amr", OutputFormat::Amr),
        ("wv", OutputFormat::Wavpack),
        ("avi", OutputFormat::Avi),
        ("mpegts", OutputFormat::MpegTs),
        ("flv", OutputFormat::Flv),
    ] {
        if names.contains(&name) {
            return Some(format);
        }
    }
    if names.contains(&"asf") {
        return Some(if operation == Operation::AudioConverter {
            OutputFormat::Wma
        } else {
            OutputFormat::Asf
        });
    }
    if names.contains(&"ogg") {
        return Some(
            if source
                .streams
                .iter()
                .filter(|stream| stream.kind == crate::StreamKind::Audio)
                .all(|stream| stream.codec == "opus")
            {
                OutputFormat::Opus
            } else {
                OutputFormat::Ogg
            },
        );
    }
    if names.contains(&"webm") && !names.contains(&"matroska") {
        return Some(OutputFormat::Webm);
    }
    if names.contains(&"matroska") && !names.contains(&"webm") {
        return Some(OutputFormat::Matroska);
    }
    None
}

pub(crate) fn resolve_format(
    source: &MediaInfo,
    config: &super::ConversionConfig,
) -> Result<OutputFormat, EngineError> {
    if config.format == OutputFormat::Original {
        original_format(source,config.operation).ok_or_else(|| invalid("The original representation cannot yet be identified reliably. Choose an explicit compatible output format."))
    } else {
        Ok(config.format)
    }
}

/// Automatic considers the container's predefined defaults without accepting
/// a route that cannot preserve the source representation.
pub(crate) fn video_encoding_settings(
    format: OutputFormat,
    requested: VideoCodec,
    source: &crate::VideoInfo,
    source_codec: &str,
    quality: u8,
) -> Result<crate::processing::encoding::EncodingSettings, EngineError> {
    use crate::processing::encoding::video_settings;
    if requested != VideoCodec::Automatic {
        return video_settings(requested, source, quality);
    }
    let codecs = format.definition().video_codecs;
    let mut last = None;
    for codec in codecs
        .iter()
        .filter(|codec| codec.codec_name() == Some(source_codec))
        .chain(
            codecs
                .iter()
                .filter(|codec| codec.codec_name() != Some(source_codec)),
        )
    {
        match video_settings(*codec, source, quality) {
            Ok(settings) if super::encoder_accepted(format, &settings.encoder) => {
                return Ok(settings)
            }
            Ok(_) => continue,
            Err(error) => last = Some(error),
        }
    }
    Err(last.unwrap_or_else(|| {
        invalid("No compatible video encoder is available for this representation.")
    }))
}
