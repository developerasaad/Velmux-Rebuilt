use super::sys;
use crate::EngineError;
use std::{
    collections::HashMap,
    ffi::{c_char, c_int, c_void, CString},
    ptr,
    sync::{LazyLock, Mutex},
};

const MAX_CONFIG_VALUES: usize = 1024;
const MAX_NAME_BYTES: usize = 128;
const MAX_CACHE_ENTRIES: usize = 512;

/// Declared formats of a compiled encoder, not proof that a hardware session
/// can execute. An empty applicable list means FFmpeg declares no restriction.
#[derive(Clone, Debug)]
pub(crate) struct EncoderFormats {
    pub codec_name: String,
    pub pixel_formats: Vec<String>,
    pub sample_formats: Vec<String>,
    pub sample_rates: Vec<u32>,
    pub channel_layouts: Vec<String>,
}

static FORMATS: LazyLock<Mutex<HashMap<String, Option<EncoderFormats>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Copy immutable library declarations once per encoder. No codec is opened,
/// no media is probed, and no native format/layout pointer escapes this module.
pub(crate) fn encoder_formats(name: &str) -> Result<Option<EncoderFormats>, EngineError> {
    if name.is_empty() || name.len() > MAX_NAME_BYTES || !name.is_ascii() {
        return Err(EngineError::Configuration {
            detail: "The encoder identifier is invalid.".into(),
        });
    }
    let native_name = CString::new(name).map_err(|_| EngineError::Configuration {
        detail: "The encoder identifier contains a null character.".into(),
    })?;
    let mut cache = FORMATS
        .lock()
        .map_err(|_| invalid("The encoder-format cache is unavailable."))?;
    if let Some(formats) = cache.get(name) {
        return Ok(formats.clone());
    }
    let formats = unsafe { inspect_encoder(&native_name)? };
    // Internal callers use the predefined catalog's small encoder set. Bound
    // negative-query retention as well, rather than accumulating arbitrary names.
    if cache.len() >= MAX_CACHE_ENTRIES {
        cache.retain(|_, value| value.is_some());
    }
    if formats.is_some() && cache.len() >= MAX_CACHE_ENTRIES {
        return Err(invalid(
            "The compiled encoder registry exceeds its cache bound.",
        ));
    }
    if cache.len() < MAX_CACHE_ENTRIES {
        cache.insert(name.into(), formats.clone());
    }
    Ok(formats)
}

/// Validate accepted representations against cached compiled declarations.
/// This does not open an encoder or establish hardware-route availability.
pub(crate) fn require_video_format(name: &str, pixel: &str) -> Result<(), EngineError> {
    let formats = encoder_formats(name)?.ok_or_else(|| unsupported(name))?;
    if !formats.pixel_formats.is_empty()
        && !formats.pixel_formats.iter().any(|value| value == pixel)
    {
        return Err(EngineError::Configuration {
            detail: format!(
                "{} cannot encode the accepted {pixel} pixel representation.",
                formats.codec_name
            ),
        });
    }
    Ok(())
}
pub(crate) fn require_audio_format(
    name: &str,
    sample: &str,
    rate: u32,
    layout: &str,
) -> Result<(), EngineError> {
    let formats = encoder_formats(name)?.ok_or_else(|| unsupported(name))?;
    if !formats.sample_formats.is_empty()
        && !formats.sample_formats.iter().any(|value| value == sample)
        || !formats.sample_rates.is_empty() && !formats.sample_rates.contains(&rate)
        || !formats.channel_layouts.is_empty()
            && !formats.channel_layouts.iter().any(|value| value == layout)
    {
        return Err(EngineError::Configuration {
            detail: format!(
                "{} cannot encode the accepted {sample}, {rate} Hz, {layout} audio representation.",
                formats.codec_name
            ),
        });
    }
    Ok(())
}
fn unsupported(name: &str) -> EngineError {
    EngineError::UnsupportedMedia {
        detail: format!("The required {name} encoder is unavailable in this build."),
    }
}

fn invalid(detail: impl Into<String>) -> EngineError {
    EngineError::Internal {
        detail: detail.into(),
    }
}

unsafe fn copied_name(value: *const c_char) -> Result<String, EngineError> {
    if value.is_null() {
        return Err(invalid(
            "A compiled encoder returned an unknown format name.",
        ));
    }
    let mut bytes = Vec::new();
    for position in 0..MAX_NAME_BYTES {
        let byte = *value.add(position) as u8;
        if byte == 0 {
            if bytes.is_empty() {
                return Err(invalid("A compiled encoder returned an empty format name."));
            }
            return String::from_utf8(bytes)
                .map_err(|_| invalid("A compiled encoder returned an invalid UTF-8 format name."));
        }
        bytes.push(byte);
    }
    Err(invalid(
        "A compiled encoder format name exceeds its bounds.",
    ))
}

/// Each caller pairs T with the documented AVCodecConfig representation. The
/// library owns the correctly aligned, immutable array; copy only its bounded
/// reported elements, excluding the native terminator.
unsafe fn values<T: Copy>(
    codec: *const sys::AVCodec,
    configuration: sys::AVCodecConfig,
) -> Result<Vec<T>, EngineError> {
    let mut data: *const c_void = ptr::null();
    let mut count: c_int = 0;
    let result = sys::avcodec_get_supported_config(
        ptr::null(),
        codec,
        configuration,
        0,
        &mut data,
        &mut count,
    );
    if result < 0 {
        return Err(invalid(format!(
            "A compiled encoder format query failed with FFmpeg error {result}."
        )));
    }
    if count < 0 || count as usize > MAX_CONFIG_VALUES {
        return Err(invalid(
            "A compiled encoder format list exceeds its bounds.",
        ));
    }
    if data.is_null() {
        if count != 0 {
            return Err(invalid(
                "A compiled encoder returned a missing format list.",
            ));
        }
        return Ok(Vec::new());
    }
    if count == 0 {
        return Ok(Vec::new());
    }
    if !(data as usize).is_multiple_of(std::mem::align_of::<T>()) {
        return Err(invalid(
            "A compiled encoder returned a misaligned format list.",
        ));
    }
    Ok(std::slice::from_raw_parts(data.cast::<T>(), count as usize).to_vec())
}

unsafe fn inspect_encoder(name: &CString) -> Result<Option<EncoderFormats>, EngineError> {
    let codec = sys::avcodec_find_encoder_by_name(name.as_ptr());
    if codec.is_null() {
        return Ok(None);
    }
    let mut formats = EncoderFormats {
        codec_name: copied_name((*codec).name)?,
        pixel_formats: Vec::new(),
        sample_formats: Vec::new(),
        sample_rates: Vec::new(),
        channel_layouts: Vec::new(),
    };
    if (*codec).type_ == sys::AVMEDIA_TYPE_VIDEO {
        for format in values::<sys::AVPixelFormat>(codec, sys::AV_CODEC_CONFIG_PIX_FORMAT)? {
            formats
                .pixel_formats
                .push(copied_name(sys::av_get_pix_fmt_name(format))?);
        }
    } else if (*codec).type_ == sys::AVMEDIA_TYPE_AUDIO {
        for format in values::<sys::AVSampleFormat>(codec, sys::AV_CODEC_CONFIG_SAMPLE_FORMAT)? {
            formats
                .sample_formats
                .push(copied_name(sys::av_get_sample_fmt_name(format))?);
        }
        for rate in values::<c_int>(codec, sys::AV_CODEC_CONFIG_SAMPLE_RATE)? {
            if rate <= 0 {
                return Err(invalid(
                    "A compiled encoder declared an invalid sample rate.",
                ));
            }
            formats.sample_rates.push(rate as u32);
        }
        for layout in values::<sys::AVChannelLayout>(codec, sys::AV_CODEC_CONFIG_CHANNEL_LAYOUT)? {
            if layout.nb_channels <= 0
                || layout.nb_channels > 256
                || sys::av_channel_layout_check(&layout) != 1
            {
                return Err(invalid(
                    "A compiled encoder declared an invalid channel layout.",
                ));
            }
            let mut description = [0 as c_char; 512];
            let length = sys::av_channel_layout_describe(
                &layout,
                description.as_mut_ptr(),
                description.len(),
            );
            if length <= 1 || length as usize > description.len() {
                return Err(invalid(
                    "A compiled encoder channel-layout description is invalid.",
                ));
            }
            // FFmpeg reports the required size including the terminating NUL.
            let text_length = length as usize - 1;
            if description[text_length] != 0 || description[..text_length].contains(&0) {
                return Err(invalid(
                    "A compiled encoder channel-layout description is malformed.",
                ));
            }
            let bytes = description[..text_length]
                .iter()
                .map(|byte| *byte as u8)
                .collect();
            formats
                .channel_layouts
                .push(String::from_utf8(bytes).map_err(|_| {
                    invalid("A compiled encoder channel-layout description is invalid UTF-8.")
                })?);
        }
    }
    Ok(Some(formats))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn x264_declares_real_video_pixel_formats() {
        let mut formats = encoder_formats("libx264").unwrap().unwrap();
        assert_eq!(formats.codec_name, "libx264");
        assert!(formats.pixel_formats.iter().any(|value| value == "yuv420p"));
        assert!(formats.sample_formats.is_empty());
        assert!(formats.sample_rates.is_empty());
        assert!(formats.channel_layouts.is_empty());
        formats.pixel_formats.clear();
        assert!(!encoder_formats("libx264")
            .unwrap()
            .unwrap()
            .pixel_formats
            .is_empty());
    }

    #[test]
    fn aac_declares_real_sampling_and_sample_format() {
        let formats = encoder_formats("aac").unwrap().unwrap();
        assert_eq!(formats.codec_name, "aac");
        assert!(formats.pixel_formats.is_empty());
        assert!(formats.sample_formats.iter().any(|value| value == "fltp"));
        assert!(formats.sample_rates.contains(&44_100));
        assert!(formats.sample_rates.contains(&48_000));
        assert!(!formats.sample_rates.contains(&1));
    }

    #[test]
    fn flac_declares_integer_sample_representations() {
        let formats = encoder_formats("flac").unwrap().unwrap();
        assert!(formats.pixel_formats.is_empty());
        assert!(formats.sample_formats.iter().any(|value| value == "s16"));
        assert!(formats.sample_formats.iter().any(|value| value == "s32"));
        assert!(!formats.sample_formats.iter().any(|value| value == "fltp"));
    }

    #[test]
    fn mp3_layout_descriptions_are_owned_text_without_native_terminators() {
        let formats = encoder_formats("libmp3lame").unwrap().unwrap();
        assert!(formats.channel_layouts.iter().any(|value| value == "mono"));
        assert!(formats
            .channel_layouts
            .iter()
            .any(|value| value == "stereo"));
        assert!(formats
            .channel_layouts
            .iter()
            .all(|value| !value.contains('\0')));
    }

    #[test]
    fn missing_and_invalid_encoder_identifiers_do_not_invent_capabilities() {
        assert!(encoder_formats("velmux_encoder_that_does_not_exist")
            .unwrap()
            .is_none());
        assert!(matches!(
            encoder_formats("aac\0extra"),
            Err(EngineError::Configuration { .. })
        ));
        assert!(matches!(
            encoder_formats(""),
            Err(EngineError::Configuration { .. })
        ));
    }
}
