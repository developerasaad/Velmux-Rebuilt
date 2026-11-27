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
};

const EOF: i32 = -541478725;
const AGAIN: i32 = -libc::EAGAIN;

fn text(value: &str) -> Result<CString, EngineError> {
    CString::new(value).map_err(|_| EngineError::Configuration {
        detail: "An option contains an invalid NUL character.".into(),
    })
}
fn failure(code: i32, stage: &str) -> EngineError {
    let mut buffer = [0i8; 192];
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
    decoder: *mut sys::AVCodecContext,
    encoder: *mut sys::AVCodecContext,
    graph: *mut sys::AVFilterGraph,
    source: *mut sys::AVFilterContext,
    sink: *mut sys::AVFilterContext,
    output_stream: *mut sys::AVStream,
    packets_written: u64,
    timestamp_origin: i64,
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
) -> Result<(), EngineError> {
    for index in 0..(*from).nb_coded_side_data {
        let item = &*(*from).coded_side_data.add(index as usize);
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

unsafe fn make_transform(
    input: *mut sys::AVStream,
    out: *mut sys::AVStream,
    encoder_name: &str,
    plan: &super::StreamPlan,
    config: &crate::ConversionConfig,
    global: bool,
    origin_us: Option<i64>,
) -> Result<Transform, EngineError> {
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
        decoder: sys::avcodec_alloc_context3(decoder_codec),
        encoder: sys::avcodec_alloc_context3(encoder_codec),
        graph: ptr::null_mut(),
        source: ptr::null_mut(),
        sink: ptr::null_mut(),
        output_stream: out,
        packets_written: 0,
        timestamp_origin: origin_in_time_base(origin_us, (*input).time_base)?,
    };
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
    check(
        sys::avcodec_open2(dec, decoder_codec, ptr::null_mut()),
        "Decode",
    )?;
    (*enc).thread_count = 2;
    if global {
        (*enc).flags |= sys::AV_CODEC_FLAG_GLOBAL_HEADER as i32;
    }
    let is_video = (*dec).codec_type == sys::AVMEDIA_TYPE_VIDEO;
    if is_video {
        (*enc).width = plan.width.unwrap() as i32;
        (*enc).height = plan.height.unwrap() as i32;
        (*enc).pix_fmt = sys::AV_PIX_FMT_YUV420P;
        (*enc).sample_aspect_ratio = if (*dec).sample_aspect_ratio.num > 0 {
            (*dec).sample_aspect_ratio
        } else {
            sys::AVRational { num: 1, den: 1 }
        };
        (*enc).time_base = if config.frame_rate == FrameRate::Fps30 {
            sys::AVRational { num: 1, den: 30 }
        } else {
            (*input).time_base
        };
        (*enc).framerate = if config.frame_rate == FrameRate::Fps30 {
            sys::AVRational { num: 30, den: 1 }
        } else {
            (*input).avg_frame_rate
        };
        (*enc).color_primaries = (*dec).color_primaries;
        (*enc).color_trc = (*dec).color_trc;
        (*enc).colorspace = (*dec).colorspace;
        (*enc).color_range = (*dec).color_range;
        (*enc).chroma_sample_location = (*dec).chroma_sample_location;
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
    } else {
        (*enc).sample_rate = plan.sample_rate.unwrap() as i32;
        let channels = plan.channels.unwrap() as i32;
        if channels == (*dec).ch_layout.nb_channels {
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
        (*enc).time_base = sys::AVRational {
            num: 1,
            den: (*enc).sample_rate,
        };
        if plan.expected_codec == "aac" || plan.expected_codec == "mp3" {
            (*enc).bit_rate = i64::from(config.audio_bitrate);
        }
    }
    check(
        sys::avcodec_open2(enc, encoder_codec, ptr::null_mut()),
        "Encode",
    )?;
    check(
        sys::avcodec_parameters_from_context((*out).codecpar, enc),
        "Encode",
    )?;
    copy_side_data((*input).codecpar, (*out).codecpar)?;
    (*(*out).codecpar).codec_tag = 0;
    (*out).time_base = (*enc).time_base;
    transform.graph = sys::avfilter_graph_alloc();
    if transform.graph.is_null() {
        return Err(failure(-libc::ENOMEM, "Filter"));
    }
    (*transform.graph).nb_threads = 2;
    let (src_name, sink_name, args, chain) = if is_video {
        let sar = (*enc).sample_aspect_ratio;
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
        let fps = if config.frame_rate == FrameRate::Fps30 {
            ",fps=30"
        } else {
            ""
        };
        (
            "buffer",
            "buffersink",
            args,
            format!(
                "scale={}:{}:flags=bicubic,format=pix_fmts=yuv420p{fps}",
                (*enc).width,
                (*enc).height
            ),
        )
    } else {
        let mut in_layout = [0i8; 128];
        let mut out_layout = [0i8; 128];
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
        (
            "abuffer",
            "abuffersink",
            args,
            format!(
                "aresample={},aformat=sample_fmts={}:sample_rates={}:channel_layouts={}",
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
    ) -> Result<(), EngineError> {
        loop {
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
            if token.is_cancelled() {
                return Err(EngineError::Cancelled);
            }
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
            let result = sys::avcodec_send_frame(self.encoder, frame.0);
            sys::av_frame_unref(frame.0);
            check(result, "Encode")?;
            self.drain_encoder(out, packet)?;
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
            if token.is_cancelled() {
                return Err(EngineError::Cancelled);
            }
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
    mut progress: impl FnMut(i64),
) -> Result<(), EngineError> {
    let reserved = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|e| EngineError::Storage {
            detail: format!("Unable to reserve the working output: {e}"),
        })?;
    drop(reserved);
    let muxer = text(plan.config.format.muxer())?;
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
            || plan.streams.len() != (*inf).nb_streams as usize
            || source.streams.len() != plan.streams.len()
        {
            return Err(EngineError::InvalidMedia {
                detail: "The opened source no longer matches the accepted stream plan.".into(),
            });
        }
        for (position, item) in plan.streams.iter().enumerate() {
            if item.index as usize != position || item.index >= (*inf).nb_streams {
                return Err(EngineError::Configuration {
                    detail: "The accepted stream mapping is invalid.".into(),
                });
            }
            let stream = *(*inf).streams.add(item.index as usize);
            if stream.is_null()
                || (*stream).codecpar.is_null()
                || (*stream).index != item.index as i32
                || (*(*stream).codecpar).codec_id as i32 != source.streams[position].codec_id
            {
                return Err(EngineError::InvalidMedia {
                    detail: "The opened source stream identities have changed.".into(),
                });
            }
        }
        // One origin preserves relative stream offsets. Never independently
        // zero each stream, which would shift audio against video/subtitles.
        let origin_us = source.start_time_us.or_else(|| {
            source
                .streams
                .iter()
                .filter_map(|stream| stream.start_time_us)
                .min()
        });
        let out = output.context;
        let mut transforms = Vec::new();
        let mut copy_packets = vec![0u64; plan.streams.len()];
        check(
            sys::av_dict_copy(&mut (*out).metadata, (*inf).metadata, 0),
            "Mux",
        )?;
        // Chapters are copied only to an explicitly compatible representation.
        if (*inf).nb_chapters > 0 {
            if plan.config.format != crate::OutputFormat::Matroska {
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
                    &plan.config,
                    (*(*out).oformat).flags & sys::AVFMT_GLOBALHEADER as i32 != 0,
                    origin_us,
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
        let mut packet = Packet::new()?;
        let mut encoded = Packet::new()?;
        let mut frame = Frame::new()?;
        let mut filtered = Frame::new()?;
        let first = origin_us.unwrap_or(0);
        let mut furthest = 0i64;
        loop {
            input.check_cancelled()?;
            let result = sys::av_read_frame(inf, packet.0);
            if result == EOF {
                break;
            }
            input.check_result(result, "Unable to read the source during processing")?;
            let index = (*packet.0).stream_index;
            if index < 0 || index as usize >= transforms.len() {
                sys::av_packet_unref(packet.0);
                return Err(EngineError::InvalidMedia {
                    detail: "A packet refers to an unknown stream.".into(),
                });
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
                progress(furthest);
            }
            if let Some(transform) = &mut transforms[index as usize] {
                let result = sys::avcodec_send_packet(transform.decoder, packet.0);
                sys::av_packet_unref(packet.0);
                check(result, "Decode")?;
                transform.drain_decoder(out, &mut frame, &mut filtered, &mut encoded, &token)?;
            } else {
                let dest = *(*out).streams.add(index as usize);
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
                    copy_packets[index as usize] = copy_packets[index as usize].saturating_add(1);
                }
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
            transform.drain_encoder(out, &mut encoded)?;
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
