use super::Plan;
use crate::{
    ffmpeg::sys, media::input::Input, CancellationToken, EngineError, FrameRate, MediaInfo,
};
use std::{
    ffi::{CStr, CString},
    fs::{File, OpenOptions},
    os::unix::fs::OpenOptionsExt,
    path::Path,
    ptr,
    sync::Arc,
    time::{Duration, Instant},
};

const EOF: i32 = -541478725;
const AGAIN: i32 = -libc::EAGAIN;

fn text(value: &str) -> Result<CString, EngineError> {
    CString::new(value).map_err(|_| EngineError::Configuration {
        detail: "An option contains an invalid NUL character.".into(),
    })
}
fn failure(code: i32, stage: &str) -> EngineError {
    let mut buffer = [0 as std::ffi::c_char; 192];
    unsafe {
        sys::av_strerror(code, buffer.as_mut_ptr(), buffer.len());
    }
    let detail = unsafe { CStr::from_ptr(buffer.as_ptr()) }.to_string_lossy();
    let detail = format!("{stage}: {detail}");
    match stage {
        "Decode" => EngineError::Decoding { detail },
        "Filter" => EngineError::Filtering { detail },
        "Encode" => EngineError::Encoding { detail },
        _ => EngineError::Muxing { detail },
    }
}
fn check(code: i32, stage: &str) -> Result<(), EngineError> {
    if code < 0 {
        Err(failure(code, stage))
    } else {
        Ok(())
    }
}

fn origin_in_time_base(
    origin_us: Option<i64>,
    time_base: sys::AVRational,
) -> Result<i64, EngineError> {
    let Some(origin) = origin_us else {
        return Ok(0);
    };
    if time_base.num <= 0 || time_base.den <= 0 {
        return Err(EngineError::InvalidMedia {
            detail: "A stream has an invalid timestamp time base.".into(),
        });
    }
    let numerator = i128::from(origin) * i128::from(time_base.den);
    let denominator = 1_000_000i128 * i128::from(time_base.num);
    let rounded = if numerator >= 0 {
        (numerator + denominator / 2) / denominator
    } else {
        -((-numerator + denominator / 2) / denominator)
    };
    i64::try_from(rounded).map_err(|_| EngineError::InvalidMedia {
        detail: "The source timestamp origin exceeds the supported range.".into(),
    })
}

fn shift_timestamp(timestamp: i64, origin: i64) -> Result<i64, EngineError> {
    if timestamp == i64::MIN {
        return Ok(timestamp);
    }
    timestamp
        .checked_sub(origin)
        .ok_or_else(|| EngineError::InvalidMedia {
            detail: "A source timestamp exceeds the supported range.".into(),
        })
}

fn check_work(token: &CancellationToken, deadline: Option<Instant>) -> Result<(), EngineError> {
    if token.is_cancelled() {
        return Err(EngineError::Cancelled);
    }
    if deadline.is_some_and(|limit| Instant::now() >= limit) {
        return Err(EngineError::Platform {
            detail: "Preparing this bounded preview took too long. Try another position.".into(),
        });
    }
    Ok(())
}

struct Output {
    context: *mut sys::AVFormatContext,
}
impl Drop for Output {
    fn drop(&mut self) {
        unsafe {
            if !self.context.is_null() {
                if !(*self.context).pb.is_null() {
                    sys::avio_closep(&mut (*self.context).pb);
                }
                sys::avformat_free_context(self.context);
            }
        }
    }
}
struct Transform {
    constant_video_rate: bool,
    decoder: *mut sys::AVCodecContext,
    encoder: *mut sys::AVCodecContext,
    graph: *mut sys::AVFilterGraph,
    source: *mut sys::AVFilterContext,
    sink: *mut sys::AVFilterContext,
    output_stream: *mut sys::AVStream,
    packets_written: u64,
    timestamp_origin: i64,
    input_time_base: sys::AVRational,
    preview_duration: Option<i64>,
    preview_goal: Option<i64>,
    preview_finished: bool,
    tone_map: bool,
    deadline: Option<Instant>,
}
impl Drop for Transform {
    fn drop(&mut self) {
        unsafe {
            sys::avfilter_graph_free(&mut self.graph);
            sys::avcodec_free_context(&mut self.decoder);
            sys::avcodec_free_context(&mut self.encoder);
        }
    }
}
struct Packet(*mut sys::AVPacket);
impl Packet {
    fn new() -> Result<Self, EngineError> {
        let p = unsafe { sys::av_packet_alloc() };
        if p.is_null() {
            Err(failure(-libc::ENOMEM, "Mux"))
        } else {
            Ok(Self(p))
        }
    }
}
impl Drop for Packet {
    fn drop(&mut self) {
        unsafe {
            sys::av_packet_free(&mut self.0);
        }
    }
}
struct Frame(*mut sys::AVFrame);
impl Frame {
    fn new() -> Result<Self, EngineError> {
        let p = unsafe { sys::av_frame_alloc() };
        if p.is_null() {
            Err(failure(-libc::ENOMEM, "Decode"))
        } else {
            Ok(Self(p))
        }
    }
}
impl Drop for Frame {
    fn drop(&mut self) {
        unsafe {
            sys::av_frame_free(&mut self.0);
        }
    }
}

unsafe fn copy_side_data(
    from: *const sys::AVCodecParameters,
    to: *mut sys::AVCodecParameters,
    strip_hdr: bool,
) -> Result<(), EngineError> {
    for index in 0..(*from).nb_coded_side_data {
        let item = &*(*from).coded_side_data.add(index as usize);
        if strip_hdr
            && [
                sys::AV_PKT_DATA_MASTERING_DISPLAY_METADATA,
                sys::AV_PKT_DATA_CONTENT_LIGHT_LEVEL,
                sys::AV_PKT_DATA_DOVI_CONF,
                sys::AV_PKT_DATA_DYNAMIC_HDR10_PLUS,
            ]
            .contains(&item.type_)
        {
            continue;
        }
        let allocated = sys::av_packet_side_data_new(
            &mut (*to).coded_side_data,
            &mut (*to).nb_coded_side_data,
            item.type_,
            item.size,
            0,
        );
        if allocated.is_null() {
            return Err(failure(-libc::ENOMEM, "Mux"));
        }
        ptr::copy_nonoverlapping(item.data, (*allocated).data, item.size);
    }
    Ok(())
}

/// Supply static HDR metadata before encoder initialization. The packet and
/// frame representations use the same documented payload structs, but their
/// enum types and allocation ownership are different.
unsafe fn copy_hdr_encoder_side_data(
    from: *const sys::AVCodecParameters,
    to: *mut sys::AVCodecContext,
    strip_hdr: bool,
) -> Result<(), EngineError> {
    if strip_hdr {
        return Ok(());
    }
    if (*from).nb_coded_side_data < 0
        || (*from).nb_coded_side_data > 1024
        || (*from).nb_coded_side_data > 0 && (*from).coded_side_data.is_null()
    {
        return Err(EngineError::InvalidMedia {
            detail: "The source HDR side-data declaration is invalid.".into(),
        });
    }
    for index in 0..(*from).nb_coded_side_data {
        let source = &*(*from).coded_side_data.add(index as usize);
        let (kind, size) = match source.type_ {
            sys::AV_PKT_DATA_MASTERING_DISPLAY_METADATA => (
                sys::AV_FRAME_DATA_MASTERING_DISPLAY_METADATA,
                std::mem::size_of::<sys::AVMasteringDisplayMetadata>(),
            ),
            sys::AV_PKT_DATA_CONTENT_LIGHT_LEVEL => (
                sys::AV_FRAME_DATA_CONTENT_LIGHT_LEVEL,
                std::mem::size_of::<sys::AVContentLightMetadata>(),
            ),
            _ => continue,
        };
        if source.data.is_null() || source.size < size {
            return Err(EngineError::InvalidMedia {
                detail: "The source HDR metadata payload is incomplete.".into(),
            });
        }
        if !sys::av_frame_side_data_get_c(
            (*to).decoded_side_data.cast(),
            (*to).nb_decoded_side_data,
            kind,
        )
        .is_null()
        {
            return Err(EngineError::InvalidMedia {
                detail: "The source declares duplicate static HDR metadata.".into(),
            });
        }
        if kind == sys::AV_FRAME_DATA_MASTERING_DISPLAY_METADATA {
            let metadata =
                ptr::read_unaligned(source.data.cast::<sys::AVMasteringDisplayMetadata>());
            let valid_coordinate =
                |value: sys::AVRational| value.den > 0 && value.num >= 0 && value.num <= value.den;
            let valid_luminance = |value: sys::AVRational| {
                value.den > 0
                    && value.num >= 0
                    && u64::from(value.num as u32) * 10_000
                        <= u64::from(u32::MAX) * value.den as u64
            };
            if metadata.has_primaries != 0
                && (!metadata
                    .display_primaries
                    .iter()
                    .flatten()
                    .copied()
                    .all(valid_coordinate)
                    || !metadata.white_point.iter().copied().all(valid_coordinate))
                || metadata.has_luminance != 0
                    && (!valid_luminance(metadata.min_luminance)
                        || !valid_luminance(metadata.max_luminance)
                        || i64::from(metadata.min_luminance.num)
                            * i64::from(metadata.max_luminance.den)
                            > i64::from(metadata.max_luminance.num)
                                * i64::from(metadata.min_luminance.den))
            {
                return Err(EngineError::InvalidMedia {
                    detail: "The source mastering-display metadata contains invalid values.".into(),
                });
            }
            // The pinned x265 wrapper consumes every field without consulting
            // these flags. Reject incomplete input rather than inventing the
            // missing chromaticities/luminance or passing invalid rationals.
            if (*to).codec_id == sys::AV_CODEC_ID_HEVC
                && (metadata.has_primaries == 0 || metadata.has_luminance == 0)
            {
                return Err(EngineError::UnsupportedMedia {
                    detail: "This HEVC encoder cannot faithfully preserve incomplete mastering-display metadata. Use compatible stream copying or a supported representation.".into(),
                });
            }
        } else {
            let metadata = ptr::read_unaligned(source.data.cast::<sys::AVContentLightMetadata>());
            if metadata.MaxCLL > u32::from(u16::MAX) || metadata.MaxFALL > u32::from(u16::MAX) {
                return Err(EngineError::InvalidMedia {
                    detail: "The source content-light metadata exceeds its encoded bounds.".into(),
                });
            }
        }
        // FFmpeg allocates a fresh reference-counted buffer. The encoder
        // context owns this array on success and avcodec_free_context releases
        // it on every initialization/error path; no source buffer is borrowed.
        let destination = sys::av_frame_side_data_new(
            &mut (*to).decoded_side_data,
            &mut (*to).nb_decoded_side_data,
            kind,
            size,
            0,
        );
        if destination.is_null() {
            return Err(failure(-libc::ENOMEM, "Encode"));
        }
        ptr::copy_nonoverlapping(source.data, (*destination).data, size);
    }
    Ok(())
}

struct TransformContext<'a> {
    config: &'a crate::ConversionConfig,
    global: bool,
    origin_us: Option<i64>,
    preview: Option<&'a super::PreviewRange>,
    deadline: Option<Instant>,
}

unsafe fn make_transform(
    input: *mut sys::AVStream,
    out: *mut sys::AVStream,
    encoder_name: &str,
    plan: &super::StreamPlan,
    context: &TransformContext<'_>,
) -> Result<Transform, EngineError> {
    let TransformContext {
        config,
        global,
        origin_us,
        preview,
        deadline,
    } = *context;
    let decoder_codec = sys::avcodec_find_decoder((*(*input).codecpar).codec_id);
    if decoder_codec.is_null() {
        return Err(EngineError::UnsupportedMedia {
            detail: "No decoder is available for this stream.".into(),
        });
    }
    let encoder_name = text(encoder_name)?;
    let encoder_codec = sys::avcodec_find_encoder_by_name(encoder_name.as_ptr());
    if encoder_codec.is_null() {
        return Err(EngineError::UnsupportedMedia {
            detail: "No encoder is available for this stream.".into(),
        });
    }
    let mut transform = Transform {
        constant_video_rate: plan.frame_rate.is_some() || config.frame_rate != FrameRate::Original,
        decoder: sys::avcodec_alloc_context3(decoder_codec),
        encoder: sys::avcodec_alloc_context3(encoder_codec),
        graph: ptr::null_mut(),
        source: ptr::null_mut(),
        sink: ptr::null_mut(),
        output_stream: out,
        packets_written: 0,
        timestamp_origin: origin_in_time_base(origin_us, (*input).time_base)?,
        input_time_base: (*input).time_base,
        preview_duration: preview.map(|range| range.duration_us),
        preview_goal: None,
        preview_finished: false,
        tone_map: false,
        deadline,
    };
    if let Some(range) = preview {
        let end = origin_in_time_base(Some(range.duration_us), (*input).time_base)?;
        let known_end = if (*input).duration > 0 && (*input).start_time != i64::MIN {
            (*input)
                .start_time
                .checked_add((*input).duration)
                .and_then(|value| value.checked_sub(transform.timestamp_origin))
        } else {
            None
        };
        transform.preview_goal = Some(known_end.map_or(end, |value| value.min(end)));
    }
    if transform.decoder.is_null() || transform.encoder.is_null() {
        return Err(failure(-libc::ENOMEM, "Encode"));
    }
    let dec = transform.decoder;
    let enc = transform.encoder;
    check(
        sys::avcodec_parameters_to_context(dec, (*input).codecpar),
        "Decode",
    )?;
    (*dec).pkt_timebase = (*input).time_base;
    (*dec).thread_count = 2;
    (*dec).max_pixels = 64 * 1024 * 1024;
    (*dec).max_samples = 16 * 1024 * 1024;
    if (*dec).codec_type == sys::AVMEDIA_TYPE_AUDIO
        && (*dec).ch_layout.order == sys::AV_CHANNEL_ORDER_UNSPEC
        && matches!((*dec).ch_layout.nb_channels, 1 | 2)
    {
        // Unmasked mono/stereo PCM has ordered samples but no explicit speaker
        // mask. Resolve the conventional layout without mixing or dropping them.
        let count = (*dec).ch_layout.nb_channels;
        sys::av_channel_layout_uninit(&mut (*dec).ch_layout);
        sys::av_channel_layout_default(&mut (*dec).ch_layout, count);
    }
    check(
        sys::avcodec_open2(dec, decoder_codec, ptr::null_mut()),
        "Decode",
    )?;
    (*enc).thread_count = 2;
    if global {
        (*enc).flags |= sys::AV_CODEC_FLAG_GLOBAL_HEADER as i32;
    }
    let is_video = (*dec).codec_type == sys::AVMEDIA_TYPE_VIDEO;
    transform.tone_map = preview.is_some()
        && is_video
        && [sys::AVCOL_TRC_SMPTE2084, sys::AVCOL_TRC_ARIB_STD_B67].contains(&(*dec).color_trc);
    if is_video {
        (*enc).flags |= sys::AV_CODEC_FLAG_FRAME_DURATION as i32;
        (*enc).width = plan.width.unwrap() as i32;
        (*enc).height = plan.height.unwrap() as i32;
        (*enc).pix_fmt = if let Some(settings) = &plan.encoding {
            let name = text(
                settings
                    .pixel_format
                    .as_deref()
                    .ok_or_else(|| failure(-libc::EINVAL, "Encode"))?,
            )?;
            let format = sys::av_get_pix_fmt(name.as_ptr());
            if format == sys::AV_PIX_FMT_NONE {
                return Err(failure(-libc::EINVAL, "Encode"));
            }
            format
        } else {
            sys::AV_PIX_FMT_YUV420P
        };
        let source_sar =
            if (*input).sample_aspect_ratio.num > 0 && (*input).sample_aspect_ratio.den > 0 {
                (*input).sample_aspect_ratio
            } else if (*dec).sample_aspect_ratio.num > 0 && (*dec).sample_aspect_ratio.den > 0 {
                (*dec).sample_aspect_ratio
            } else {
                sys::AVRational { num: 1, den: 1 }
            };
        (*enc).sample_aspect_ratio = source_sar;
        (*enc).sample_aspect_ratio = sys::av_mul_q(
            (*enc).sample_aspect_ratio,
            sys::AVRational {
                num: (*dec).width.checked_mul((*enc).height).ok_or_else(|| {
                    EngineError::InvalidMedia {
                        detail: "Video aspect-ratio arithmetic exceeded its bounds.".into(),
                    }
                })?,
                den: (*dec).height.checked_mul((*enc).width).ok_or_else(|| {
                    EngineError::InvalidMedia {
                        detail: "Video aspect-ratio arithmetic exceeded its bounds.".into(),
                    }
                })?,
            },
        );
        let requested_rate = plan.frame_rate.clone().or(config
            .frame_rate
            .rational(config.custom_frame_rate.as_ref())?);
        (*enc).time_base =
            requested_rate
                .as_ref()
                .map_or((*input).time_base, |rate| sys::AVRational {
                    num: rate.denominator,
                    den: rate.numerator,
                });
        (*enc).framerate = requested_rate
            .as_ref()
            .map_or((*input).avg_frame_rate, |rate| sys::AVRational {
                num: rate.numerator,
                den: rate.denominator,
            });
        (*enc).color_primaries = (*dec).color_primaries;
        (*enc).color_trc = (*dec).color_trc;
        (*enc).colorspace = (*dec).colorspace;
        (*enc).color_range = (*dec).color_range;
        (*enc).chroma_sample_location = (*dec).chroma_sample_location;
        if transform.tone_map {
            (*enc).color_primaries = sys::AVCOL_PRI_BT709;
            (*enc).color_trc = sys::AVCOL_TRC_BT709;
            (*enc).colorspace = sys::AVCOL_SPC_BT709;
            (*enc).color_range = sys::AVCOL_RANGE_MPEG;
        }
        if let Some(settings) = &plan.encoding {
            // Values were resolved by the core before acceptance. These options
            // belong to FFmpeg's context or encoder, never external user input.
            if let Some(space) = &settings.color_space {
                check(
                    sys::av_opt_set_int(
                        enc.cast(),
                        c"colorspace".as_ptr(),
                        i64::from(sys::av_color_space_from_name(text(space)?.as_ptr())),
                        0,
                    ),
                    "Encode",
                )?;
            }
            if let Some(range) = &settings.color_range {
                check(
                    sys::av_opt_set_int(
                        enc.cast(),
                        c"color_range".as_ptr(),
                        i64::from(sys::av_color_range_from_name(text(range)?.as_ptr())),
                        0,
                    ),
                    "Encode",
                )?;
            }
            if settings.codec == "mpeg4" && (*enc).time_base.den > 65535 {
                // MPEG-4 Part 2 stores a 16-bit time increment resolution.
                // Preserve presentation times on a sufficiently fine legal base.
                (*enc).time_base = sys::AVRational { num: 1, den: 60000 };
            }
        } else {
            // Already accepted M2 plans retain their original encoding policy.
            check(
                sys::av_opt_set(
                    (*enc).priv_data,
                    c"preset".as_ptr(),
                    c"veryfast".as_ptr(),
                    0,
                ),
                "Encode",
            )?;
            check(
                sys::av_opt_set(
                    (*enc).priv_data,
                    c"crf".as_ptr(),
                    text(&config.quality.to_string())?.as_ptr(),
                    0,
                ),
                "Encode",
            )?;
        }
    } else {
        (*enc).sample_rate = plan.sample_rate.unwrap() as i32;
        let channels = plan.channels.unwrap() as i32;
        if let Some(settings) = &plan.encoding {
            let layout = text(
                settings
                    .channel_layout
                    .as_deref()
                    .ok_or_else(|| failure(-libc::EINVAL, "Encode"))?,
            )?;
            check(
                sys::av_channel_layout_from_string(&mut (*enc).ch_layout, layout.as_ptr()),
                "Encode",
            )?;
            if (*enc).ch_layout.nb_channels != channels {
                return Err(failure(-libc::EINVAL, "Encode"));
            }
            let sample = text(
                settings
                    .sample_format
                    .as_deref()
                    .ok_or_else(|| failure(-libc::EINVAL, "Encode"))?,
            )?;
            (*enc).sample_fmt = sys::av_get_sample_fmt(sample.as_ptr());
            if (*enc).sample_fmt == sys::AV_SAMPLE_FMT_NONE {
                return Err(failure(-libc::EINVAL, "Encode"));
            }
            if let Some(depth) = settings.bit_depth {
                (*enc).bits_per_raw_sample = depth as i32;
            }
        } else {
            if config.channels.is_none() && channels == (*dec).ch_layout.nb_channels {
                check(
                    sys::av_channel_layout_copy(&mut (*enc).ch_layout, &(*dec).ch_layout),
                    "Encode",
                )?;
            } else {
                sys::av_channel_layout_default(&mut (*enc).ch_layout, channels);
            }
            (*enc).sample_fmt = if plan.expected_codec == "pcm_s16le" {
                sys::AV_SAMPLE_FMT_S16
            } else if plan.expected_codec == "flac" {
                let precision = plan.bit_depth.unwrap_or_else(|| {
                    let coded = sys::av_get_bits_per_sample((*dec).codec_id);
                    if (*dec).bits_per_raw_sample > 0 {
                        (*dec).bits_per_raw_sample as u32
                    } else if coded > 0 {
                        coded as u32
                    } else {
                        24
                    }
                });
                if !(1..=24).contains(&precision) {
                    return Err(EngineError::Configuration {
                        detail: "FLAC encoding requires a supported precision of at most 24 bits."
                            .into(),
                    });
                }
                (*enc).bits_per_raw_sample = precision as i32;
                if precision > 16 {
                    sys::AV_SAMPLE_FMT_S32
                } else {
                    sys::AV_SAMPLE_FMT_S16
                }
            } else {
                sys::AV_SAMPLE_FMT_FLTP
            };
        }
        (*enc).time_base = sys::AVRational {
            num: 1,
            den: (*enc).sample_rate,
        };
        if plan.encoding.is_none() && (plan.expected_codec == "aac" || plan.expected_codec == "mp3")
        {
            (*enc).bit_rate = i64::from(config.audio_bitrate.unwrap_or_else(|| {
                crate::configuration::automatic_audio_bitrate(
                    &plan.expected_codec,
                    (*enc).sample_rate as u32,
                    channels as u32,
                )
            }));
        }
    }
    if let Some(settings) = &plan.encoding {
        if let Some(rate) = settings.bit_rate {
            (*enc).bit_rate = i64::try_from(rate).map_err(|_| failure(-libc::EINVAL, "Encode"))?;
        }
        if let Some(qscale) = settings.qscale {
            (*enc).flags |= sys::AV_CODEC_FLAG_QSCALE as i32;
            (*enc).global_quality = i32::try_from(qscale)
                .ok()
                .and_then(|value| value.checked_mul(sys::FF_QP2LAMBDA as i32))
                .ok_or_else(|| failure(-libc::EINVAL, "Encode"))?;
        }
        for option in &settings.options {
            check(
                sys::av_opt_set(
                    enc.cast(),
                    text(&option.key)?.as_ptr(),
                    text(&option.value)?.as_ptr(),
                    sys::AV_OPT_SEARCH_CHILDREN as i32,
                ),
                "Encode",
            )?;
        }
    }
    if is_video {
        copy_hdr_encoder_side_data((*input).codecpar, enc, transform.tone_map)?;
    }
    let opened = sys::avcodec_open2(enc, encoder_codec, ptr::null_mut());
    if opened < 0 {
        let mut layout = [0 as std::ffi::c_char; 128];
        sys::av_channel_layout_describe(&(*enc).ch_layout, layout.as_mut_ptr(), layout.len());
        return Err(EngineError::Encoding {
            detail: format!(
                "Cannot initialize {} ({}×{}, {} Hz, {}, sample format {}): {}",
                encoder_name.to_string_lossy(),
                (*enc).width,
                (*enc).height,
                (*enc).sample_rate,
                CStr::from_ptr(layout.as_ptr()).to_string_lossy(),
                (*enc).sample_fmt,
                failure(opened, "Encode")
            ),
        });
    }
    check(
        sys::avcodec_parameters_from_context((*out).codecpar, enc),
        "Encode",
    )?;
    copy_side_data((*input).codecpar, (*out).codecpar, transform.tone_map)?;
    (*(*out).codecpar).codec_tag = 0;
    (*out).time_base = (*enc).time_base;
    if is_video {
        (*out).sample_aspect_ratio = (*enc).sample_aspect_ratio;
    }
    transform.graph = sys::avfilter_graph_alloc();
    if transform.graph.is_null() {
        return Err(failure(-libc::ENOMEM, "Filter"));
    }
    (*transform.graph).nb_threads = 2;
    let (src_name, sink_name, args, chain) = if is_video {
        // The buffer describes decoded source pixels. Scale computes the
        // destination SAR; passing its already-adjusted SAR here applies the
        // alignment compensation twice.
        let sar = if (*input).sample_aspect_ratio.num > 0 && (*input).sample_aspect_ratio.den > 0 {
            (*input).sample_aspect_ratio
        } else if (*dec).sample_aspect_ratio.num > 0 && (*dec).sample_aspect_ratio.den > 0 {
            (*dec).sample_aspect_ratio
        } else {
            sys::AVRational { num: 1, den: 1 }
        };
        let args = format!(
            "video_size={}x{}:pix_fmt={}:time_base={}/{}:pixel_aspect={}/{}",
            (*dec).width,
            (*dec).height,
            (*dec).pix_fmt,
            (*input).time_base.num,
            (*input).time_base.den,
            sar.num,
            sar.den
        );
        let fps = plan
            .frame_rate
            .clone()
            .or(config
                .frame_rate
                .rational(config.custom_frame_rate.as_ref())?)
            .map_or_else(String::new, |rate| {
                format!(",fps=fps={}/{}", rate.numerator, rate.denominator)
            });
        let trim = preview.map_or_else(String::new, |range| {
            format!(
                "trim=start=0:end={:.6},",
                range.duration_us as f64 / 1_000_000.0
            )
        });
        let tone = if transform.tone_map {
            "zscale=t=linear:npl=100,format=gbrpf32le,zscale=p=bt709,tonemap=tonemap=hable:desat=0,zscale=t=bt709:m=bt709:r=limited,format=yuv420p,"
        } else {
            ""
        };
        let pixel_name = sys::av_get_pix_fmt_name((*enc).pix_fmt);
        if pixel_name.is_null() {
            return Err(failure(-libc::EINVAL, "Filter"));
        }
        let mut color = String::new();
        if let Some(settings) = &plan.encoding {
            if let Some(space) = &settings.color_space {
                // The scale option aliases are not av_color_space_name names
                // (notably bt2020nc and smpte170m). Use the accepted enum value.
                let matrix = sys::av_color_space_from_name(text(space)?.as_ptr());
                if matrix < 0 {
                    return Err(failure(-libc::EINVAL, "Filter"));
                }
                color.push_str(&format!(":out_color_matrix={matrix}"));
            }
            if let Some(range) = &settings.color_range {
                color.push_str(&format!(":out_range={range}"));
            }
        }
        (
            "buffer",
            "buffersink",
            args,
            format!(
                "{trim}{tone}scale={}:{}:flags=bicubic{color},format=pix_fmts={}{fps}",
                (*enc).width,
                (*enc).height,
                CStr::from_ptr(pixel_name).to_string_lossy()
            ),
        )
    } else {
        let mut in_layout = [0 as std::ffi::c_char; 128];
        let mut out_layout = [0 as std::ffi::c_char; 128];
        check(
            sys::av_channel_layout_describe(
                &(*dec).ch_layout,
                in_layout.as_mut_ptr(),
                in_layout.len(),
            ),
            "Filter",
        )?;
        check(
            sys::av_channel_layout_describe(
                &(*enc).ch_layout,
                out_layout.as_mut_ptr(),
                out_layout.len(),
            ),
            "Filter",
        )?;
        let sample_name = sys::av_get_sample_fmt_name((*dec).sample_fmt);
        let output_sample = sys::av_get_sample_fmt_name((*enc).sample_fmt);
        if sample_name.is_null() || output_sample.is_null() {
            return Err(failure(-libc::EINVAL, "Filter"));
        }
        let args = format!(
            "time_base={}/{}:sample_rate={}:sample_fmt={}:channel_layout={}",
            (*input).time_base.num,
            (*input).time_base.den,
            (*dec).sample_rate,
            CStr::from_ptr(sample_name).to_string_lossy(),
            CStr::from_ptr(in_layout.as_ptr()).to_string_lossy()
        );
        let trim = preview.map_or_else(String::new, |range| {
            format!(
                "atrim=start=0:end={:.6},",
                range.duration_us as f64 / 1_000_000.0
            )
        });
        (
            "abuffer",
            "abuffersink",
            args,
            format!(
                "{trim}aresample={},aformat=sample_fmts={}:sample_rates={}:channel_layouts={}",
                (*enc).sample_rate,
                CStr::from_ptr(output_sample).to_string_lossy(),
                (*enc).sample_rate,
                CStr::from_ptr(out_layout.as_ptr()).to_string_lossy()
            ),
        )
    };
    let source_filter = sys::avfilter_get_by_name(text(src_name)?.as_ptr());
    let sink_filter = sys::avfilter_get_by_name(text(sink_name)?.as_ptr());
    if source_filter.is_null() || sink_filter.is_null() {
        return Err(EngineError::Filtering {
            detail: "Required processing filters are unavailable.".into(),
        });
    }
    check(
        sys::avfilter_graph_create_filter(
            &mut transform.source,
            source_filter,
            c"input".as_ptr(),
            text(&args)?.as_ptr(),
            ptr::null_mut(),
            transform.graph,
        ),
        "Filter",
    )?;
    check(
        sys::avfilter_graph_create_filter(
            &mut transform.sink,
            sink_filter,
            c"output".as_ptr(),
            ptr::null(),
            ptr::null_mut(),
            transform.graph,
        ),
        "Filter",
    )?;
    let mut inputs = sys::avfilter_inout_alloc();
    let mut outputs = sys::avfilter_inout_alloc();
    if inputs.is_null() || outputs.is_null() {
        sys::avfilter_inout_free(&mut inputs);
        sys::avfilter_inout_free(&mut outputs);
        return Err(failure(-libc::ENOMEM, "Filter"));
    }
    (*outputs).name = sys::av_strdup(c"in".as_ptr());
    (*outputs).filter_ctx = transform.source;
    (*outputs).pad_idx = 0;
    (*outputs).next = ptr::null_mut();
    (*inputs).name = sys::av_strdup(c"out".as_ptr());
    (*inputs).filter_ctx = transform.sink;
    (*inputs).pad_idx = 0;
    (*inputs).next = ptr::null_mut();
    let result = sys::avfilter_graph_parse_ptr(
        transform.graph,
        text(&chain)?.as_ptr(),
        &mut inputs,
        &mut outputs,
        ptr::null_mut(),
    );
    sys::avfilter_inout_free(&mut inputs);
    sys::avfilter_inout_free(&mut outputs);
    check(result, "Filter")?;
    check(
        sys::avfilter_graph_config(transform.graph, ptr::null_mut()),
        "Filter",
    )?;
    if !is_video && (*enc).frame_size > 0 {
        sys::av_buffersink_set_frame_size(transform.sink, (*enc).frame_size as u32);
    }
    Ok(transform)
}

impl Transform {
    unsafe fn drain_encoder(
        &mut self,
        out: *mut sys::AVFormatContext,
        packet: &mut Packet,
        token: &CancellationToken,
    ) -> Result<(), EngineError> {
        loop {
            check_work(token, self.deadline)?;
            let result = sys::avcodec_receive_packet(self.encoder, packet.0);
            if result == AGAIN || result == EOF {
                return Ok(());
            }
            check(result, "Encode")?;
            sys::av_packet_rescale_ts(
                packet.0,
                (*self.encoder).time_base,
                (*self.output_stream).time_base,
            );
            (*packet.0).stream_index = (*self.output_stream).index;
            let has_payload = (*packet.0).size > 0;
            let result = sys::av_interleaved_write_frame(out, packet.0);
            sys::av_packet_unref(packet.0);
            check(result, "Mux")?;
            if has_payload {
                self.packets_written = self.packets_written.saturating_add(1);
            }
        }
    }
    unsafe fn drain_filter(
        &mut self,
        out: *mut sys::AVFormatContext,
        frame: &mut Frame,
        packet: &mut Packet,
        token: &CancellationToken,
    ) -> Result<(), EngineError> {
        loop {
            check_work(token, self.deadline)?;
            let result = sys::av_buffersink_get_frame(self.sink, frame.0);
            if result == AGAIN || result == EOF {
                return Ok(());
            }
            check(result, "Filter")?;
            if (*frame.0).pts != i64::MIN {
                (*frame.0).pts = sys::av_rescale_q(
                    (*frame.0).pts,
                    sys::av_buffersink_get_time_base(self.sink),
                    (*self.encoder).time_base,
                );
            }
            // libx264 retains each frame's duration in its reordered packet.
            // Rescale that field together with PTS; a chosen CFR has exactly
            // one encoder tick even when the fps filter's final duration is 0.
            (*frame.0).duration = if (*self.encoder).codec_type == sys::AVMEDIA_TYPE_VIDEO
                && self.constant_video_rate
            {
                let rate = (*self.encoder).framerate;
                if rate.num <= 0 || rate.den <= 0 {
                    return Err(failure(-libc::EINVAL, "Encode"));
                }
                sys::av_rescale_q(
                    1,
                    sys::AVRational {
                        num: rate.den,
                        den: rate.num,
                    },
                    (*self.encoder).time_base,
                )
                .max(1)
            } else if (*frame.0).duration > 0 {
                sys::av_rescale_q(
                    (*frame.0).duration,
                    sys::av_buffersink_get_time_base(self.sink),
                    (*self.encoder).time_base,
                )
            } else {
                0
            };
            if self.tone_map {
                for kind in [
                    sys::AV_FRAME_DATA_MASTERING_DISPLAY_METADATA,
                    sys::AV_FRAME_DATA_CONTENT_LIGHT_LEVEL,
                    sys::AV_FRAME_DATA_DYNAMIC_HDR_PLUS,
                    sys::AV_FRAME_DATA_DOVI_RPU_BUFFER,
                    sys::AV_FRAME_DATA_DOVI_METADATA,
                    sys::AV_FRAME_DATA_DYNAMIC_HDR_VIVID,
                ] {
                    sys::av_frame_remove_side_data(frame.0, kind);
                }
                (*frame.0).color_primaries = sys::AVCOL_PRI_BT709;
                (*frame.0).color_trc = sys::AVCOL_TRC_BT709;
                (*frame.0).colorspace = sys::AVCOL_SPC_BT709;
                (*frame.0).color_range = sys::AVCOL_RANGE_MPEG;
            }
            if (*self.encoder).flags & sys::AV_CODEC_FLAG_QSCALE as i32 != 0 {
                (*frame.0).quality = (*self.encoder).global_quality;
            }
            if (*self.encoder).codec_type == sys::AVMEDIA_TYPE_VIDEO {
                // Decoder frame types belong to the source GOP. The selected
                // encoder must choose its own references and keyframes.
                (*frame.0).pict_type = sys::AV_PICTURE_TYPE_NONE;
            }
            let result = sys::avcodec_send_frame(self.encoder, frame.0);
            sys::av_frame_unref(frame.0);
            check(result, "Encode")?;
            self.drain_encoder(out, packet, token)?;
        }
    }
    unsafe fn drain_decoder(
        &mut self,
        out: *mut sys::AVFormatContext,
        frame: &mut Frame,
        filtered: &mut Frame,
        packet: &mut Packet,
        token: &CancellationToken,
    ) -> Result<(), EngineError> {
        loop {
            check_work(token, self.deadline)?;
            let result = sys::avcodec_receive_frame(self.decoder, frame.0);
            if result == AGAIN || result == EOF {
                return Ok(());
            }
            check(result, "Decode")?;
            if (*frame.0).best_effort_timestamp != i64::MIN {
                (*frame.0).pts = (*frame.0).best_effort_timestamp;
            }
            if (*frame.0).pts == i64::MIN {
                sys::av_frame_unref(frame.0);
                return Err(EngineError::InvalidMedia {
                    detail: "A decoded frame has no usable presentation timestamp.".into(),
                });
            }
            (*frame.0).pts = shift_timestamp((*frame.0).pts, self.timestamp_origin)?;
            if let Some(duration_us) = self.preview_duration {
                let end = origin_in_time_base(Some(duration_us), self.input_time_base)?;
                if (*frame.0).pts >= end {
                    self.preview_finished = true;
                    sys::av_frame_unref(frame.0);
                    continue;
                }
                let is_audio = (*self.decoder).codec_type == sys::AVMEDIA_TYPE_AUDIO;
                let frame_duration = if is_audio && (*self.decoder).sample_rate > 0 {
                    sys::av_rescale_q(
                        i64::from((*frame.0).nb_samples),
                        sys::AVRational {
                            num: 1,
                            den: (*self.decoder).sample_rate,
                        },
                        self.input_time_base,
                    )
                } else {
                    (*frame.0).duration.max(0)
                };
                if self.preview_goal.is_some_and(|goal| {
                    goal < end && (*frame.0).pts.saturating_add(frame_duration) >= goal
                }) {
                    self.preview_finished = true;
                }
                if !is_audio && (*frame.0).pts < 0 {
                    sys::av_frame_unref(frame.0);
                    continue;
                }
            }
            let result = sys::av_buffersrc_add_frame_flags(
                self.source,
                frame.0,
                sys::AV_BUFFERSRC_FLAG_KEEP_REF as i32,
            );
            sys::av_frame_unref(frame.0);
            check(result, "Filter")?;
            self.drain_filter(out, filtered, packet, token)?;
        }
    }
}

/// Incremental demux/decode/filter/encode/mux. Every native resource stays on
/// this worker; no frame buffers or FFmpeg pointers cross the public boundary.
pub(crate) fn convert(
    input: Input,
    source: &MediaInfo,
    plan: &Plan,
    path: &Path,
    token: Arc<CancellationToken>,
    mut progress: impl FnMut(super::ProcessingProgress) -> Result<(), EngineError>,
) -> Result<(), EngineError> {
    let representation = plan.output()?;
    let deadline = plan
        .preview_range
        .as_ref()
        .map(|_| Instant::now() + Duration::from_secs(30));
    check_work(&token, deadline)?;
    let reserved = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|e| EngineError::Storage {
            detail: format!("Unable to reserve the working output: {e}"),
        })?;
    drop(reserved);
    let muxer = text(&representation.muxer)?;
    let path_text = text(path.to_str().ok_or_else(|| EngineError::Storage {
        detail: "The working path is not valid Unicode.".into(),
    })?)?;
    let mut output = Output {
        context: ptr::null_mut(),
    };
    unsafe {
        check(
            sys::avformat_alloc_output_context2(
                &mut output.context,
                ptr::null(),
                muxer.as_ptr(),
                path_text.as_ptr(),
            ),
            "Mux",
        )?;
        if output.context.is_null() {
            return Err(failure(-libc::ENOMEM, "Mux"));
        }
        let inf = input.format();
        if inf.is_null()
            || (*inf).streams.is_null()
            || source.streams.len() != (*inf).nb_streams as usize
            || plan.streams.is_empty()
            || (plan.preview_range.is_none() && plan.streams.len() != (*inf).nb_streams as usize)
        {
            return Err(EngineError::InvalidMedia {
                detail: "The opened source no longer matches the accepted stream plan.".into(),
            });
        }
        let mut source_to_output = vec![None; (*inf).nb_streams as usize];
        for (position, item) in plan.streams.iter().enumerate() {
            if item.index >= (*inf).nb_streams
                || source_to_output[item.index as usize].is_some()
                || (plan.preview_range.is_none() && item.index as usize != position)
            {
                return Err(EngineError::Configuration {
                    detail: "The accepted stream mapping is invalid.".into(),
                });
            }
            let stream = *(*inf).streams.add(item.index as usize);
            if stream.is_null()
                || (*stream).codecpar.is_null()
                || (*stream).index != item.index as i32
                || (*(*stream).codecpar).codec_id as i32
                    != source.streams[item.index as usize].codec_id
            {
                return Err(EngineError::InvalidMedia {
                    detail: "The opened source stream identities have changed.".into(),
                });
            }
            source_to_output[item.index as usize] = Some(position);
        }
        // One origin preserves relative stream offsets. Previews additionally
        // subtract the selected logical range start from every selected track.
        let source_origin = super::common_origin(source);
        let origin_us = if let Some(range) = &plan.preview_range {
            if range.start_us < 0 || !(1..=15_000_000).contains(&range.duration_us) {
                return Err(EngineError::Configuration {
                    detail: "The preview range is invalid.".into(),
                });
            }
            Some(
                source_origin
                    .unwrap_or(0)
                    .checked_add(range.start_us)
                    .ok_or_else(|| EngineError::Configuration {
                        detail: "The preview origin exceeds the supported range.".into(),
                    })?,
            )
        } else {
            source_origin
        };
        if plan
            .preview_range
            .as_ref()
            .is_some_and(|range| range.start_us > 0)
        {
            check_work(&token, deadline)?;
            let result = sys::av_seek_frame(
                inf,
                -1,
                origin_us.unwrap(),
                sys::AVSEEK_FLAG_BACKWARD as i32,
            );
            if result < 0 {
                return Err(EngineError::UnsupportedMedia {
                    detail: "This source cannot seek reliably to the requested preview position."
                        .into(),
                });
            }
        }
        let out = output.context;
        let mut transforms = Vec::new();
        let mut copy_packets = vec![0u64; plan.streams.len()];
        check(
            sys::av_dict_copy(&mut (*out).metadata, (*inf).metadata, 0),
            "Mux",
        )?;
        // Chapters are copied only to an explicitly compatible representation.
        if (*inf).nb_chapters > 0 && plan.preview_range.is_none() {
            if representation.format != crate::OutputFormat::Matroska {
                return Err(EngineError::Configuration {
                    detail: "Choose Matroska to preserve source chapters in this foundation."
                        .into(),
                });
            }
            (*out).chapters = sys::av_mallocz(
                (*inf).nb_chapters as usize * std::mem::size_of::<*mut sys::AVChapter>(),
            )
            .cast();
            if (*out).chapters.is_null() {
                return Err(failure(-libc::ENOMEM, "Mux"));
            }
            for n in 0..(*inf).nb_chapters as usize {
                let old = *(*inf).chapters.add(n);
                let new =
                    sys::av_mallocz(std::mem::size_of::<sys::AVChapter>()).cast::<sys::AVChapter>();
                if new.is_null() {
                    return Err(failure(-libc::ENOMEM, "Mux"));
                }
                *(*out).chapters.add(n) = new;
                (*out).nb_chapters += 1;
                (*new).id = (*old).id;
                (*new).time_base = (*old).time_base;
                let origin = origin_in_time_base(origin_us, (*old).time_base)?;
                (*new).start = shift_timestamp((*old).start, origin)?;
                (*new).end = shift_timestamp((*old).end, origin)?;
                check(
                    sys::av_dict_copy(&mut (*new).metadata, (*old).metadata, 0),
                    "Mux",
                )?;
            }
        }
        for item in &plan.streams {
            let src = *(*inf).streams.add(item.index as usize);
            let dest = sys::avformat_new_stream(out, ptr::null());
            if dest.is_null() {
                return Err(failure(-libc::ENOMEM, "Mux"));
            }
            (*dest).id = (*src).id;
            (*dest).disposition = (*src).disposition;
            (*dest).sample_aspect_ratio = (*src).sample_aspect_ratio;
            check(
                sys::av_dict_copy(&mut (*dest).metadata, (*src).metadata, 0),
                "Mux",
            )?;
            if let Some(encoder) = &item.encoder {
                transforms.push(Some(make_transform(
                    src,
                    dest,
                    encoder,
                    item,
                    &TransformContext {
                        config: &plan.config,
                        global: (*(*out).oformat).flags & sys::AVFMT_GLOBALHEADER as i32 != 0,
                        origin_us,
                        preview: plan.preview_range.as_ref(),
                        deadline,
                    },
                )?));
            } else {
                check(
                    sys::avcodec_parameters_copy((*dest).codecpar, (*src).codecpar),
                    "Mux",
                )?;
                (*(*dest).codecpar).codec_tag = 0;
                (*dest).time_base = (*src).time_base;
                transforms.push(None);
            }
        }
        check(
            sys::avio_open(
                &mut (*out).pb,
                path_text.as_ptr(),
                sys::AVIO_FLAG_WRITE as i32,
            ),
            "Mux",
        )?;
        check(sys::avformat_write_header(out, ptr::null_mut()), "Mux")?;
        let packet = Packet::new()?;
        let mut encoded = Packet::new()?;
        let mut frame = Frame::new()?;
        let mut filtered = Frame::new()?;
        let first = origin_us.unwrap_or(0);
        let mut furthest = 0i64;
        let mut packets_read = 0usize;
        loop {
            input.check_cancelled()?;
            check_work(&token, deadline)?;
            if plan.preview_range.is_some() && packets_read >= 50_000 {
                return Err(EngineError::Platform { detail: "This source requires too much scanning for a bounded preview. Try another position.".into() });
            }
            let result = sys::av_read_frame(inf, packet.0);
            if result == EOF {
                break;
            }
            input.check_result(result, "Unable to read the source during processing")?;
            packets_read += 1;
            let index = (*packet.0).stream_index;
            if index < 0 || index as usize >= source_to_output.len() {
                sys::av_packet_unref(packet.0);
                return Err(EngineError::InvalidMedia {
                    detail: "A packet refers to an unknown stream.".into(),
                });
            }
            let Some(output_index) = source_to_output[index as usize] else {
                sys::av_packet_unref(packet.0);
                continue;
            };
            if plan.preview_range.is_some()
                && transforms[output_index]
                    .as_ref()
                    .is_some_and(|transform| transform.preview_finished)
            {
                sys::av_packet_unref(packet.0);
                continue;
            }
            let src = *(*inf).streams.add(index as usize);
            if (*packet.0).pts != i64::MIN {
                let timestamp = sys::av_rescale_q(
                    (*packet.0).pts,
                    (*src).time_base,
                    sys::AVRational {
                        num: 1,
                        den: 1_000_000,
                    },
                );
                furthest = furthest.max(timestamp.saturating_sub(first));
                progress(super::ProcessingProgress::Timestamp(furthest))?;
            }
            if let Some(transform) = &mut transforms[output_index] {
                let result = sys::avcodec_send_packet(transform.decoder, packet.0);
                sys::av_packet_unref(packet.0);
                check(result, "Decode")?;
                transform.drain_decoder(out, &mut frame, &mut filtered, &mut encoded, &token)?;
            } else {
                let dest = *(*out).streams.add(output_index);
                let origin = origin_in_time_base(origin_us, (*src).time_base)?;
                (*packet.0).pts = shift_timestamp((*packet.0).pts, origin)?;
                (*packet.0).dts = shift_timestamp((*packet.0).dts, origin)?;
                sys::av_packet_rescale_ts(packet.0, (*src).time_base, (*dest).time_base);
                (*packet.0).pos = -1;
                let has_payload = (*packet.0).size > 0;
                let result = sys::av_interleaved_write_frame(out, packet.0);
                sys::av_packet_unref(packet.0);
                check(result, "Mux")?;
                if has_payload {
                    copy_packets[output_index] = copy_packets[output_index].saturating_add(1);
                }
            }
            if plan.preview_range.is_some()
                && transforms.iter().all(|transform| {
                    transform
                        .as_ref()
                        .is_some_and(|transform| transform.preview_finished)
                })
            {
                break;
            }
        }
        for transform in transforms.iter_mut().flatten() {
            input.check_cancelled()?;
            check(
                sys::avcodec_send_packet(transform.decoder, ptr::null()),
                "Decode",
            )?;
            transform.drain_decoder(out, &mut frame, &mut filtered, &mut encoded, &token)?;
            check(
                sys::av_buffersrc_add_frame_flags(transform.source, ptr::null_mut(), 0),
                "Filter",
            )?;
            transform.drain_filter(out, &mut filtered, &mut encoded, &token)?;
            check(
                sys::avcodec_send_frame(transform.encoder, ptr::null()),
                "Encode",
            )?;
            transform.drain_encoder(out, &mut encoded, &token)?;
        }
        input.check_cancelled()?;
        for (index, item) in plan.streams.iter().enumerate() {
            let source_stream = *(*inf).streams.add(item.index as usize);
            let kind = (*(*source_stream).codecpar).codec_type;
            let primary = kind == sys::AVMEDIA_TYPE_AUDIO
                || (kind == sys::AVMEDIA_TYPE_VIDEO
                    && (*source_stream).disposition & sys::AV_DISPOSITION_ATTACHED_PIC as i32 == 0);
            let written = transforms[index]
                .as_ref()
                .map_or(copy_packets[index], |transform| transform.packets_written);
            if primary && written == 0 {
                return Err(EngineError::InvalidMedia {
                    detail: format!("Output stream {index} contains no processed media packets."),
                });
            }
        }
        if input.source_identity()? != source.source_identity {
            return Err(EngineError::SourceAccess {
                detail: "The source changed during processing. Select it again before retrying."
                    .into(),
            });
        }
        progress(super::ProcessingProgress::Finalizing)?;
        check(sys::av_write_trailer(out), "Mux")?;
        sys::avio_flush((*out).pb);
        check((*(*out).pb).error, "Mux")?;
        check(sys::avio_closep(&mut (*out).pb), "Mux")?;
    }
    File::open(path)
        .and_then(|file| file.sync_all())
        .map_err(|e| EngineError::Storage {
            detail: format!("Unable to commit the finalized working output: {e}"),
        })?;
    Ok(())
}

#[cfg(test)]
mod timestamp_tests {
    use super::*;

    struct HdrFixture {
        source: *mut sys::AVCodecParameters,
        encoder: *mut sys::AVCodecContext,
    }
    impl HdrFixture {
        fn new() -> Self {
            let fixture = unsafe {
                Self {
                    source: sys::avcodec_parameters_alloc(),
                    encoder: sys::avcodec_alloc_context3(ptr::null()),
                }
            };
            assert!(!fixture.source.is_null() && !fixture.encoder.is_null());
            fixture
        }
        unsafe fn add<T>(&self, kind: sys::AVPacketSideDataType, value: &T) -> *mut u8 {
            let side = sys::av_packet_side_data_new(
                &mut (*self.source).coded_side_data,
                &mut (*self.source).nb_coded_side_data,
                kind,
                std::mem::size_of::<T>(),
                0,
            );
            assert!(!side.is_null());
            ptr::copy_nonoverlapping(
                (value as *const T).cast::<u8>(),
                (*side).data,
                std::mem::size_of::<T>(),
            );
            (*side).data
        }
    }
    impl Drop for HdrFixture {
        fn drop(&mut self) {
            unsafe {
                sys::avcodec_parameters_free(&mut self.source);
                sys::avcodec_free_context(&mut self.encoder);
            }
        }
    }

    #[test]
    fn hdr_encoder_initialization_owns_independent_static_metadata() {
        let fixture = HdrFixture::new();
        unsafe {
            (*fixture.encoder).codec_id = sys::AV_CODEC_ID_HEVC;
            let mut mastering: sys::AVMasteringDisplayMetadata = std::mem::zeroed();
            mastering.display_primaries = [[sys::AVRational { num: 1, den: 2 }; 2]; 3];
            mastering.white_point = [sys::AVRational { num: 1, den: 2 }; 2];
            mastering.min_luminance = sys::AVRational { num: 1, den: 10000 };
            mastering.max_luminance = sys::AVRational { num: 1000, den: 1 };
            mastering.has_primaries = 1;
            mastering.has_luminance = 1;
            fixture.add(sys::AV_PKT_DATA_MASTERING_DISPLAY_METADATA, &mastering);
            let source_light = fixture.add(
                sys::AV_PKT_DATA_CONTENT_LIGHT_LEVEL,
                &sys::AVContentLightMetadata {
                    MaxCLL: 1000,
                    MaxFALL: 400,
                },
            );
            copy_hdr_encoder_side_data(fixture.source, fixture.encoder, false).unwrap();
            assert_eq!((*fixture.encoder).nb_decoded_side_data, 2);
            let copied = sys::av_frame_side_data_get_c(
                (*fixture.encoder).decoded_side_data.cast(),
                (*fixture.encoder).nb_decoded_side_data,
                sys::AV_FRAME_DATA_CONTENT_LIGHT_LEVEL,
            );
            assert!(!copied.is_null());
            assert_ne!((*copied).data, source_light);
            ptr::write_unaligned(
                source_light.cast::<sys::AVContentLightMetadata>(),
                sys::AVContentLightMetadata {
                    MaxCLL: 2000,
                    MaxFALL: 800,
                },
            );
            let light = ptr::read_unaligned((*copied).data.cast::<sys::AVContentLightMetadata>());
            assert_eq!((light.MaxCLL, light.MaxFALL), (1000, 400));
            // Cleanup also exercises an encoder context that was never opened.
        }
    }

    #[test]
    fn malformed_hdr_is_rejected_and_tone_mapping_excludes_static_hdr() {
        let fixture = HdrFixture::new();
        unsafe {
            fixture.add(sys::AV_PKT_DATA_MASTERING_DISPLAY_METADATA, &0u8);
            copy_hdr_encoder_side_data(fixture.source, fixture.encoder, true).unwrap();
            assert_eq!((*fixture.encoder).nb_decoded_side_data, 0);
            assert!(matches!(
                copy_hdr_encoder_side_data(fixture.source, fixture.encoder, false),
                Err(EngineError::InvalidMedia { .. })
            ));
        }
        let fixture = HdrFixture::new();
        unsafe {
            let mut mastering: sys::AVMasteringDisplayMetadata = std::mem::zeroed();
            mastering.has_primaries = 1;
            fixture.add(sys::AV_PKT_DATA_MASTERING_DISPLAY_METADATA, &mastering);
            assert!(matches!(
                copy_hdr_encoder_side_data(fixture.source, fixture.encoder, false),
                Err(EngineError::InvalidMedia { .. })
            ));
            assert_eq!((*fixture.encoder).nb_decoded_side_data, 0);
        }
    }

    #[test]
    fn common_positive_origin_retains_relative_offsets_and_encoder_delay() {
        let origin = Some(10_000_000);
        let video_origin = origin_in_time_base(
            origin,
            sys::AVRational {
                num: 1,
                den: 90_000,
            },
        )
        .unwrap();
        let audio_origin = origin_in_time_base(
            origin,
            sys::AVRational {
                num: 1,
                den: 48_000,
            },
        )
        .unwrap();
        assert_eq!(shift_timestamp(900_000, video_origin).unwrap(), 0);
        assert_eq!(shift_timestamp(484_800, audio_origin).unwrap(), 4_800);
        assert_eq!(shift_timestamp(478_976, audio_origin).unwrap(), -1_024);
        assert_eq!(shift_timestamp(i64::MIN, video_origin).unwrap(), i64::MIN);
    }

    #[test]
    fn origin_scaling_rejects_invalid_bases_and_arithmetic_overflow() {
        assert!(origin_in_time_base(Some(1), sys::AVRational { num: 0, den: 1 }).is_err());
        assert!(shift_timestamp(i64::MAX, -1).is_err());
        assert_eq!(
            origin_in_time_base(None, sys::AVRational { num: 0, den: 0 }).unwrap(),
            0
        );
    }
}
