use super::{
    input::Input, AudioInfo, ChapterInfo, ContentLightInfo, MasteringDisplayInfo, MediaInfo,
    MetadataEntry, Rational, StreamInfo, StreamKind, SubtitleInfo, SubtitleRepresentation,
    VideoInfo,
};
use crate::{
    api::{CancellationToken, EngineError, MediaProvider, OpenedSource},
    ffmpeg::sys,
};
use std::{
    ffi::{c_char, CStr},
    mem,
    path::Path,
    ptr,
    sync::Arc,
};

const MAX_CHAPTERS: usize = 4096;
const MAX_METADATA_ENTRIES: usize = 256;
const MAX_METADATA_BYTES: usize = 4096;

pub(crate) fn inspect(
    source: OpenedSource,
    provider: Arc<dyn MediaProvider>,
    cancellation: Arc<CancellationToken>,
) -> Result<MediaInfo, EngineError> {
    let input = Input::open(source, provider, cancellation)?;
    inspect_input(&input)
}

pub(crate) fn inspect_file(
    path: &Path,
    cancellation: Arc<CancellationToken>,
) -> Result<MediaInfo, EngineError> {
    let input = Input::open_file(path, cancellation)?;
    inspect_input(&input)
}

pub(crate) fn inspect_input(input: &Input) -> Result<MediaInfo, EngineError> {
    input.check_cancelled()?;
    let identity = input.source_identity()?;
    // Input exclusively owns the live format and all pointers read below.
    // Values are copied into Rust records before the input can be dropped.
    unsafe {
        let format = &*input.format();
        if format.iformat.is_null() || format.streams.is_null() {
            return Err(invalid("The source did not provide a media description"));
        }
        let descriptor = &*format.iformat;
        let mut streams = Vec::with_capacity(format.nb_streams as usize);
        for index in 0..format.nb_streams as usize {
            input.check_cancelled()?;
            let stream = *format.streams.add(index);
            if stream.is_null() || (*stream).codecpar.is_null() {
                return Err(invalid("The source contains an invalid stream description"));
            }
            streams.push(stream_info(&*stream, index, &identity));
        }
        if format.nb_chapters as usize > MAX_CHAPTERS {
            return Err(invalid(
                "The source contains too many chapters to inspect safely",
            ));
        }
        let mut chapters = Vec::with_capacity(format.nb_chapters as usize);
        if format.nb_chapters > 0 && format.chapters.is_null() {
            return Err(invalid("The source contains invalid chapter data"));
        }
        for index in 0..format.nb_chapters as usize {
            input.check_cancelled()?;
            let chapter = *format.chapters.add(index);
            if chapter.is_null() {
                return Err(invalid("The source contains an invalid chapter"));
            }
            let chapter = &*chapter;
            chapters.push(ChapterInfo {
                id: chapter.id,
                start_time_us: timestamp(chapter.start, chapter.time_base),
                end_time_us: timestamp(chapter.end, chapter.time_base),
                metadata: metadata(chapter.metadata),
            });
        }
        let container = text(descriptor.name).unwrap_or_else(|| "unknown".into());
        let start_time_us = (format.start_time != i64::MIN)
            .then_some(format.start_time)
            .or_else(|| {
                streams
                    .iter()
                    .filter_map(|stream| stream.start_time_us)
                    .min()
            });
        Ok(MediaInfo {
            source_identity: identity,
            byte_size: input.byte_size(),
            duration_us: content_duration(
                &container,
                input.header_duration_us(),
                (format.duration >= 0).then_some(format.duration),
                start_time_us,
            ),
            container,
            container_long_name: text(descriptor.long_name),
            start_time_us,
            bit_rate: positive_i64(format.bit_rate),
            streams,
            metadata: metadata(format.metadata),
            chapters,
            staged: input.was_staged(),
        })
    }
}

fn content_duration(
    container: &str,
    header_duration_us: Option<i64>,
    duration_us: Option<i64>,
    start_time_us: Option<i64>,
) -> Option<i64> {
    // Matroska SegmentInfo.Duration is the segment timeline endpoint, copied
    // unchanged by matroskadec. matroskaenc records max(packet PTS + duration),
    // including an initial timestamp offset. Our logical content timeline starts
    // at the same common origin used by conversion and preview. Other demuxers
    // (and FFmpeg's generic timing estimates) already report elapsed duration.
    if container
        .split(',')
        .any(|name| matches!(name, "matroska" | "webm"))
    {
        if let Some(endpoint) = header_duration_us {
            return endpoint
                .checked_sub(start_time_us?)
                .filter(|duration| *duration >= 0);
        }
    }
    duration_us
}

unsafe fn stream_info(stream: &sys::AVStream, index: usize, identity: &str) -> StreamInfo {
    let parameters = &*stream.codecpar;
    let descriptor = sys::avcodec_descriptor_get(parameters.codec_id);
    let codec =
        text(sys::avcodec_get_name(parameters.codec_id)).unwrap_or_else(|| "unknown".into());
    let kind = match parameters.codec_type {
        sys::AVMEDIA_TYPE_VIDEO => StreamKind::Video,
        sys::AVMEDIA_TYPE_AUDIO => StreamKind::Audio,
        sys::AVMEDIA_TYPE_SUBTITLE => StreamKind::Subtitle,
        sys::AVMEDIA_TYPE_DATA => StreamKind::Data,
        sys::AVMEDIA_TYPE_ATTACHMENT => StreamKind::Attachment,
        _ => StreamKind::Unknown,
    };
    let video = (kind == StreamKind::Video).then(|| video_info(stream, parameters));
    let audio = (kind == StreamKind::Audio).then(|| audio_info(parameters));
    let subtitle = (kind == StreamKind::Subtitle).then(|| {
        let properties = if descriptor.is_null() {
            0
        } else {
            (*descriptor).props as u32
        };
        SubtitleInfo {
            representation: if properties & sys::AV_CODEC_PROP_BITMAP_SUB != 0 {
                SubtitleRepresentation::Bitmap
            } else if properties & sys::AV_CODEC_PROP_TEXT_SUB != 0 {
                SubtitleRepresentation::Text
            } else {
                SubtitleRepresentation::Unknown
            },
        }
    });
    StreamInfo {
        id: format!("{identity}:{index}:{}", stream.id),
        index: index as u32,
        container_id: stream.id,
        kind,
        codec_id: parameters.codec_id as i32,
        codec,
        codec_long_name: if descriptor.is_null() {
            None
        } else {
            text((*descriptor).long_name)
        },
        decoder_available: !sys::avcodec_find_decoder(parameters.codec_id).is_null(),
        time_base: rational(stream.time_base),
        duration_us: (stream.duration >= 0)
            .then(|| timestamp(stream.duration, stream.time_base))
            .flatten(),
        start_time_us: timestamp(stream.start_time, stream.time_base),
        frame_count: positive_i64(stream.nb_frames),
        bit_rate: positive_i64(parameters.bit_rate),
        disposition: stream.disposition as u32,
        profile: if parameters.profile >= 0 {
            text(sys::avcodec_profile_name(
                parameters.codec_id,
                parameters.profile,
            ))
        } else {
            None
        },
        level: (parameters.level > 0).then_some(parameters.level),
        metadata: metadata(stream.metadata),
        video,
        audio,
        subtitle,
    }
}

unsafe fn video_info(stream: &sys::AVStream, parameters: &sys::AVCodecParameters) -> VideoInfo {
    let sample_aspect =
        rational(stream.sample_aspect_ratio).or_else(|| rational(parameters.sample_aspect_ratio));
    let display_aspect_ratio = sample_aspect
        .as_ref()
        .and_then(|sar| display_aspect(parameters.width, parameters.height, sar));
    let (matrix_data, matrix_size) = side_data(stream, sys::AV_PKT_DATA_DISPLAYMATRIX);
    let display_matrix = if !matrix_data.is_null() && matrix_size >= 9 * mem::size_of::<i32>() {
        (0..9)
            .map(|index| ptr::read_unaligned(matrix_data.cast::<i32>().add(index)))
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    let rotation = if display_matrix.len() == 9 {
        let angle = sys::av_display_rotation_get(display_matrix.as_ptr());
        angle.is_finite().then_some(angle)
    } else {
        None
    };
    let mut bit_depth = positive_i32(parameters.bits_per_raw_sample);
    let pixel = sys::av_pix_fmt_desc_get(parameters.format);
    if bit_depth.is_none() && !pixel.is_null() {
        let components = (*pixel).nb_components.min(4) as usize;
        bit_depth = (&(*pixel).comp)[..components]
            .iter()
            .filter_map(|component| positive_i32(component.depth))
            .max();
    }
    let (master_data, master_size) = side_data(stream, sys::AV_PKT_DATA_MASTERING_DISPLAY_METADATA);
    let mastering_display = if !master_data.is_null()
        && master_size >= mem::size_of::<sys::AVMasteringDisplayMetadata>()
    {
        let value = ptr::read_unaligned(master_data.cast::<sys::AVMasteringDisplayMetadata>());
        Some(MasteringDisplayInfo {
            primaries: if value.has_primaries != 0 {
                value
                    .display_primaries
                    .iter()
                    .flatten()
                    .filter_map(|r| rational_value(*r))
                    .collect()
            } else {
                Vec::new()
            },
            white_point: if value.has_primaries != 0 {
                value
                    .white_point
                    .iter()
                    .filter_map(|r| rational_value(*r))
                    .collect()
            } else {
                Vec::new()
            },
            min_luminance: (value.has_luminance != 0)
                .then(|| rational_value(value.min_luminance))
                .flatten(),
            max_luminance: (value.has_luminance != 0)
                .then(|| rational_value(value.max_luminance))
                .flatten(),
        })
    } else {
        None
    };
    let (light_data, light_size) = side_data(stream, sys::AV_PKT_DATA_CONTENT_LIGHT_LEVEL);
    let content_light =
        if !light_data.is_null() && light_size >= mem::size_of::<sys::AVContentLightMetadata>() {
            let value = ptr::read_unaligned(light_data.cast::<sys::AVContentLightMetadata>());
            Some(ContentLightInfo {
                max_content_light_level: value.MaxCLL,
                max_frame_average_light_level: value.MaxFALL,
            })
        } else {
            None
        };
    VideoInfo {
        width: positive_i32(parameters.width),
        height: positive_i32(parameters.height),
        sample_aspect_ratio: sample_aspect,
        display_aspect_ratio,
        average_frame_rate: rational(stream.avg_frame_rate),
        nominal_frame_rate: rational(stream.r_frame_rate)
            .or_else(|| rational(parameters.framerate)),
        variable_frame_rate: None,
        rotation_degrees_counter_clockwise: rotation,
        display_matrix,
        pixel_format: text(sys::av_get_pix_fmt_name(parameters.format)),
        bit_depth,
        color_primaries: specified_text(
            parameters.color_primaries,
            sys::AVCOL_PRI_UNSPECIFIED,
            sys::av_color_primaries_name,
        ),
        color_transfer: specified_text(
            parameters.color_trc,
            sys::AVCOL_TRC_UNSPECIFIED,
            sys::av_color_transfer_name,
        ),
        color_space: specified_text(
            parameters.color_space,
            sys::AVCOL_SPC_UNSPECIFIED,
            sys::av_color_space_name,
        ),
        color_range: specified_text(
            parameters.color_range,
            sys::AVCOL_RANGE_UNSPECIFIED,
            sys::av_color_range_name,
        ),
        chroma_location: specified_text(
            parameters.chroma_location,
            sys::AVCHROMA_LOC_UNSPECIFIED,
            sys::av_chroma_location_name,
        ),
        attached_picture: stream.disposition & sys::AV_DISPOSITION_ATTACHED_PIC as i32 != 0,
        mastering_display,
        content_light,
    }
}

unsafe fn audio_info(parameters: &sys::AVCodecParameters) -> AudioInfo {
    let mut layout = [0 as std::ffi::c_char; 256];
    let result =
        sys::av_channel_layout_describe(&parameters.ch_layout, layout.as_mut_ptr(), layout.len());
    AudioInfo {
        sample_rate: positive_i32(parameters.sample_rate),
        channels: positive_i32(parameters.ch_layout.nb_channels),
        channel_layout: if result >= 0 && (result as usize) < layout.len() {
            text(layout.as_ptr())
        } else {
            None
        },
        sample_format: text(sys::av_get_sample_fmt_name(parameters.format)),
        bit_depth: positive_i32(parameters.bits_per_raw_sample)
            .or_else(|| positive_i32(sys::av_get_bits_per_sample(parameters.codec_id))),
        initial_padding: parameters.initial_padding.max(0) as u32,
        trailing_padding: parameters.trailing_padding.max(0) as u32,
        seek_preroll: parameters.seek_preroll.max(0) as u32,
    }
}

unsafe fn specified_text(
    value: u32,
    unspecified: u32,
    name: unsafe extern "C" fn(u32) -> *const c_char,
) -> Option<String> {
    if value == unspecified {
        None
    } else {
        text(name(value))
    }
}

unsafe fn side_data(stream: &sys::AVStream, kind: sys::AVPacketSideDataType) -> (*const u8, usize) {
    let parameters = &*stream.codecpar;
    let side = sys::av_packet_side_data_get(
        parameters.coded_side_data,
        parameters.nb_coded_side_data,
        kind,
    );
    if !side.is_null() {
        return ((*side).data.cast_const(), (*side).size);
    }
    let mut size = 0usize;
    let data = sys::av_stream_get_side_data(stream, kind, &mut size);
    (data.cast_const(), size)
}

unsafe fn metadata(dictionary: *const sys::AVDictionary) -> Vec<MetadataEntry> {
    let mut result = Vec::new();
    let mut cursor = ptr::null_mut();
    for _ in 0..MAX_METADATA_ENTRIES {
        cursor = sys::av_dict_get(
            dictionary,
            c"".as_ptr(),
            cursor,
            sys::AV_DICT_IGNORE_SUFFIX as i32,
        );
        if cursor.is_null() {
            break;
        }
        result.push(MetadataEntry {
            key: text((*cursor).key).unwrap_or_default(),
            value: text((*cursor).value).unwrap_or_default(),
        });
    }
    result
}

unsafe fn text(pointer: *const c_char) -> Option<String> {
    if pointer.is_null() {
        return None;
    }
    let bytes = CStr::from_ptr(pointer).to_bytes();
    Some(String::from_utf8_lossy(&bytes[..bytes.len().min(MAX_METADATA_BYTES)]).into_owned())
}

fn positive_i32(value: i32) -> Option<u32> {
    (value > 0).then_some(value as u32)
}
fn positive_i64(value: i64) -> Option<u64> {
    (value > 0).then_some(value as u64)
}

fn rational(value: sys::AVRational) -> Option<Rational> {
    (value.num > 0 && value.den > 0).then_some(Rational {
        numerator: value.num,
        denominator: value.den,
    })
}

fn rational_value(value: sys::AVRational) -> Option<Rational> {
    (value.den > 0).then_some(Rational {
        numerator: value.num,
        denominator: value.den,
    })
}

fn timestamp(value: i64, time_base: sys::AVRational) -> Option<i64> {
    if value == i64::MIN || time_base.num <= 0 || time_base.den <= 0 {
        return None;
    }
    let result =
        i128::from(value) * i128::from(time_base.num) * 1_000_000 / i128::from(time_base.den);
    i64::try_from(result).ok()
}

fn display_aspect(width: i32, height: i32, sar: &Rational) -> Option<Rational> {
    if width <= 0 || height <= 0 {
        return None;
    }
    let mut numerator = i64::from(width) * i64::from(sar.numerator);
    let mut denominator = i64::from(height) * i64::from(sar.denominator);
    let (mut a, mut b) = (numerator, denominator);
    while b != 0 {
        (a, b) = (b, a % b);
    }
    if a <= 0 {
        return None;
    }
    numerator /= a;
    denominator /= a;
    Some(Rational {
        numerator: i32::try_from(numerator).ok()?,
        denominator: i32::try_from(denominator).ok()?,
    })
}

fn invalid(detail: &str) -> EngineError {
    EngineError::InvalidMedia {
        detail: detail.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::content_duration;

    #[test]
    fn matroska_header_endpoint_uses_the_shared_content_origin() {
        assert_eq!(
            content_duration(
                "matroska,webm",
                Some(7_250_000),
                Some(7_250_000),
                Some(5_000_000)
            ),
            Some(2_250_000)
        );
        assert_eq!(
            content_duration(
                "matroska,webm",
                Some(2_000_000),
                Some(2_000_000),
                Some(-7_000)
            ),
            Some(2_007_000)
        );
        assert_eq!(
            content_duration("matroska,webm", Some(2_000_000), Some(2_000_000), None),
            None
        );
        assert_eq!(
            content_duration("matroska,webm", Some(2), Some(2), Some(3)),
            None
        );
    }

    #[test]
    fn elapsed_durations_are_not_shifted_again() {
        for (container, header) in [
            ("matroska,webm", None),
            ("mov,mp4,m4a,3gp,3g2,mj2", Some(2_250_000)),
            ("mpegts", Some(2_250_000)),
        ] {
            assert_eq!(
                content_duration(container, header, Some(2_250_000), Some(5_000_000)),
                Some(2_250_000)
            );
        }
    }
}
