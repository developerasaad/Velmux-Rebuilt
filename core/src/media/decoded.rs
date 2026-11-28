//! Bounded decoded HDR facts supplement container declarations during the one
//! source inspection. Ordinary configuration reads only the retained snapshot.
use super::{input::Input, inspect, MediaInfo, StreamKind, VideoInfo};
use crate::{ffmpeg::sys, EngineError};
use std::{
    ffi::CStr,
    ptr,
    time::{Duration, Instant},
};

const EOF: i32 = -541478725;
const AGAIN: i32 = -libc::EAGAIN;
const MAX_PACKETS: usize = 256;
const MAX_BYTES: usize = 16 * 1024 * 1024;

struct Decoder(*mut sys::AVCodecContext);
impl Drop for Decoder {
    fn drop(&mut self) {
        unsafe { sys::avcodec_free_context(&mut self.0) }
    }
}
struct Frame(*mut sys::AVFrame);
impl Drop for Frame {
    fn drop(&mut self) {
        unsafe { sys::av_frame_free(&mut self.0) }
    }
}
struct Packet(*mut sys::AVPacket);
impl Drop for Packet {
    fn drop(&mut self) {
        unsafe { sys::av_packet_free(&mut self.0) }
    }
}

fn limited() -> EngineError {
    EngineError::Platform {
        detail: "Inspecting the first HDR frame exceeded the bounded preparation budget. Try another source; missing metadata was not inferred.".into(),
    }
}

unsafe fn decoded(result: i32) -> Result<(), EngineError> {
    if result >= 0 {
        return Ok(());
    }
    let mut buffer = [0; 192];
    sys::av_strerror(result, buffer.as_mut_ptr(), buffer.len());
    Err(EngineError::Decoding {
        detail: format!(
            "Cannot inspect the first HDR frame: {}",
            CStr::from_ptr(buffer.as_ptr()).to_string_lossy()
        ),
    })
}

unsafe fn frame_metadata(frame: &Frame, video: &mut VideoInfo) -> Result<(), EngineError> {
    let master =
        sys::av_frame_get_side_data(frame.0, sys::AV_FRAME_DATA_MASTERING_DISPLAY_METADATA);
    if video.mastering_display.is_none() && !master.is_null() {
        video.mastering_display = inspect::mastering_metadata((*master).data, (*master).size);
        if video.mastering_display.is_none() {
            return Err(EngineError::InvalidMedia {
                detail: "The decoded mastering-display metadata is incomplete.".into(),
            });
        }
    }
    let light = sys::av_frame_get_side_data(frame.0, sys::AV_FRAME_DATA_CONTENT_LIGHT_LEVEL);
    if video.content_light.is_none() && !light.is_null() {
        video.content_light = inspect::light_metadata((*light).data, (*light).size);
        if video.content_light.is_none() {
            return Err(EngineError::InvalidMedia {
                detail: "The decoded content-light metadata is incomplete.".into(),
            });
        }
    }
    Ok(())
}

unsafe fn packet_bytes(packet: &Packet) -> Option<usize> {
    let mut bytes = usize::try_from((*packet.0).size).ok()?;
    let count = usize::try_from((*packet.0).side_data_elems).ok()?;
    if count > 1024 || count > 0 && (*packet.0).side_data.is_null() {
        return None;
    }
    for index in 0..count {
        bytes = bytes.checked_add((*(*packet.0).side_data.add(index)).size)?;
    }
    Some(bytes)
}

pub(super) fn complete_hdr_facts(
    input: &mut Input,
    info: &mut MediaInfo,
) -> Result<(), EngineError> {
    // Auxiliary cover art and additional video streams do not replace the
    // converter's primary video. SDR/audio inspections never decode here.
    let Some(stream) = info.streams.iter_mut().find(|stream| {
        stream.kind == StreamKind::Video
            && stream
                .video
                .as_ref()
                .is_some_and(|video| !video.attached_picture)
    }) else {
        return Ok(());
    };
    let video = stream.video.as_mut().unwrap();
    if !stream.decoder_available
        || video.mastering_display.is_some() && video.content_light.is_some()
        || !video
            .color_transfer
            .as_deref()
            .is_some_and(|transfer| matches!(transfer, "smpte2084" | "arib-std-b67"))
    {
        return Ok(());
    }
    input.cap_read_deadline(Instant::now() + Duration::from_secs(10));
    unsafe {
        let source = *(*input.format()).streams.add(stream.index as usize);
        let codec = sys::avcodec_find_decoder((*(*source).codecpar).codec_id);
        if codec.is_null() {
            return Ok(());
        }
        let decoder = Decoder(sys::avcodec_alloc_context3(codec));
        let frame = Frame(sys::av_frame_alloc());
        let packet = Packet(sys::av_packet_alloc());
        if decoder.0.is_null() || frame.0.is_null() || packet.0.is_null() {
            return Err(EngineError::Internal {
                detail: "Unable to allocate bounded HDR inspection resources.".into(),
            });
        }
        decoded(sys::avcodec_parameters_to_context(
            decoder.0,
            (*source).codecpar,
        ))?;
        (*decoder.0).pkt_timebase = (*source).time_base;
        (*decoder.0).thread_count = 1;
        (*decoder.0).max_pixels = 64 * 1024 * 1024;
        decoded(sys::avcodec_open2(decoder.0, codec, ptr::null_mut()))?;
        let mut count = 0;
        let mut bytes = 0usize;
        let mut eof = false;
        loop {
            input.check_cancelled()?;
            let result = sys::avcodec_receive_frame(decoder.0, frame.0);
            input.check_cancelled()?;
            if result >= 0 {
                // No static SEI is a valid result, including ordinary HLG.
                return frame_metadata(&frame, video);
            }
            if result == EOF || eof {
                return Err(EngineError::InvalidMedia {
                    detail: "The HDR source contains no decodable primary frame.".into(),
                });
            }
            if result != AGAIN {
                decoded(result)?;
            }
            if count >= MAX_PACKETS || bytes >= MAX_BYTES {
                return Err(limited());
            }
            let result = sys::av_read_frame(input.format(), packet.0);
            input.check_cancelled()?;
            if result == EOF {
                decoded(sys::avcodec_send_packet(decoder.0, ptr::null()))?;
                eof = true;
                continue;
            }
            input.check_result(
                result,
                "Unable to read the first HDR frame during inspection",
            )?;
            count += 1;
            bytes = bytes
                .checked_add(packet_bytes(&packet).ok_or_else(limited)?)
                .filter(|size| *size <= MAX_BYTES)
                .ok_or_else(limited)?;
            if (*packet.0).stream_index == (*source).index {
                decoded(sys::avcodec_send_packet(decoder.0, packet.0))?;
            }
            sys::av_packet_unref(packet.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem;

    #[test]
    fn decoded_hdr_metadata_is_owned_after_frame_release() {
        // Reuse the normal snapshot constructor instead of inventing alternate
        // metadata/rational conversion rules for decoded frames.
        unsafe {
            let parameters: sys::AVCodecParameters = mem::zeroed();
            let mut stream: sys::AVStream = mem::zeroed();
            stream.codecpar = (&parameters as *const sys::AVCodecParameters).cast_mut();
            let mut video = inspect::video_info(&stream, &parameters);
            let frame = Frame(sys::av_frame_alloc());
            assert!(!frame.0.is_null());
            let side = sys::av_frame_new_side_data(
                frame.0,
                sys::AV_FRAME_DATA_CONTENT_LIGHT_LEVEL,
                mem::size_of::<sys::AVContentLightMetadata>(),
            );
            assert!(!side.is_null());
            ptr::write_unaligned(
                (*side).data.cast::<sys::AVContentLightMetadata>(),
                sys::AVContentLightMetadata {
                    MaxCLL: 1000,
                    MaxFALL: 400,
                },
            );
            let side = sys::av_frame_new_side_data(
                frame.0,
                sys::AV_FRAME_DATA_MASTERING_DISPLAY_METADATA,
                mem::size_of::<sys::AVMasteringDisplayMetadata>(),
            );
            assert!(!side.is_null());
            let mut mastering: sys::AVMasteringDisplayMetadata = mem::zeroed();
            mastering.display_primaries = [[sys::AVRational { num: 1, den: 2 }; 2]; 3];
            mastering.white_point = [sys::AVRational { num: 1, den: 4 }; 2];
            mastering.min_luminance = sys::AVRational {
                num: 50,
                den: 10000,
            };
            mastering.max_luminance = sys::AVRational { num: 1000, den: 1 };
            mastering.has_primaries = 1;
            mastering.has_luminance = 1;
            ptr::write_unaligned(
                (*side).data.cast::<sys::AVMasteringDisplayMetadata>(),
                mastering,
            );
            frame_metadata(&frame, &mut video).unwrap();
            drop(frame);
            let light = video.content_light.unwrap();
            assert_eq!(
                (
                    light.max_content_light_level,
                    light.max_frame_average_light_level
                ),
                (1000, 400)
            );
            let mastering = video.mastering_display.unwrap();
            assert_eq!(
                mastering.primaries,
                vec![
                    super::super::Rational {
                        numerator: 1,
                        denominator: 2
                    };
                    6
                ]
            );
            assert_eq!(
                mastering.white_point,
                vec![
                    super::super::Rational {
                        numerator: 1,
                        denominator: 4
                    };
                    2
                ]
            );
            assert_eq!(
                mastering.min_luminance,
                Some(super::super::Rational {
                    numerator: 50,
                    denominator: 10000
                })
            );
            assert_eq!(
                mastering.max_luminance,
                Some(super::super::Rational {
                    numerator: 1000,
                    denominator: 1
                })
            );
        }
    }
}
