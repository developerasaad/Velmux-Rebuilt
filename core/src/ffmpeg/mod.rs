use std::{ffi::CStr, ptr};

pub(crate) mod encoders;

// These style allowances are confined to bindgen's unmodified C declarations
// and bitfield helpers; handwritten resource and processing code remains linted.
#[allow(
    dead_code,
    non_camel_case_types,
    non_snake_case,
    non_upper_case_globals,
    unnecessary_transmutes,
    clippy::ptr_offset_with_cast,
    clippy::upper_case_acronyms,
    clippy::type_complexity
)]
pub(crate) mod sys {
    include!(concat!(env!("OUT_DIR"), "/ffmpeg.rs"));
}

pub(crate) struct Capabilities {
    pub version: String,
    pub encoders: u32,
    pub decoders: u32,
    pub muxers: u32,
    pub demuxers: u32,
    pub filters: u32,
}

pub(crate) fn capabilities() -> Capabilities {
    // FFmpeg owns these immutable registries and the static, NUL-terminated version
    // string. Each iterator has its own opaque cursor; no pointers escape this call.
    unsafe {
        let version = CStr::from_ptr(sys::av_version_info())
            .to_string_lossy()
            .into_owned();
        let mut cursor = ptr::null_mut();
        let (mut encoders, mut decoders) = (0, 0);
        loop {
            let codec = sys::av_codec_iterate(&mut cursor);
            if codec.is_null() {
                break;
            }
            encoders += u32::from(sys::av_codec_is_encoder(codec) != 0);
            decoders += u32::from(sys::av_codec_is_decoder(codec) != 0);
        }
        let mut muxers = 0;
        cursor = ptr::null_mut();
        while !sys::av_muxer_iterate(&mut cursor).is_null() {
            muxers += 1;
        }
        let mut demuxers = 0;
        cursor = ptr::null_mut();
        while !sys::av_demuxer_iterate(&mut cursor).is_null() {
            demuxers += 1;
        }
        let mut filters = 0;
        cursor = ptr::null_mut();
        while !sys::av_filter_iterate(&mut cursor).is_null() {
            filters += 1;
        }
        Capabilities {
            version,
            encoders,
            decoders,
            muxers,
            demuxers,
            filters,
        }
    }
}
