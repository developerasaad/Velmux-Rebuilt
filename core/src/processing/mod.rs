mod transcode;

use crate::{configuration::*, ffmpeg::sys, EngineError, MediaInfo, StreamKind};
use serde::{Deserialize, Serialize};
use std::ffi::CString;

pub(crate) use transcode::convert;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct StreamPlan {
    pub index: u32,
    pub encoder: Option<String>,
    pub expected_codec: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub sample_rate: Option<u32>,
    pub channels: Option<u32>,
    pub bit_depth: Option<u32>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Plan {
    pub config: ConversionConfig,
    pub streams: Vec<StreamPlan>,
}

fn codec_id(name: &str) -> Result<i32, EngineError> {
    let name = CString::new(name).map_err(|_| invalid("Invalid codec name"))?;
    unsafe {
        let codec = sys::avcodec_find_encoder_by_name(name.as_ptr());
        if codec.is_null() {
            return Err(invalid(
                "The required encoder is unavailable in this build.",
            ));
        }
        Ok((*codec).id as i32)
    }
}

pub(crate) fn validate_operation(
    info: &MediaInfo,
    operation: Operation,
) -> Result<(), EngineError> {
    let video = info
        .streams
        .iter()
        .filter(|s| {
            s.kind == StreamKind::Video && !s.video.as_ref().is_some_and(|v| v.attached_picture)
        })
        .count();
    let audio = info
        .streams
        .iter()
        .filter(|s| s.kind == StreamKind::Audio)
        .count();
    if operation == Operation::VideoConverter && video != 1 {
        return Err(invalid(
            "Video Converter currently requires exactly one primary video stream.",
        ));
    }
    if operation == Operation::AudioConverter && (audio != 1 || video != 0) {
        return Err(invalid("Audio Converter currently accepts one audio stream in an audio source. Video extraction and additional track selection are not available yet."));
    }
    Ok(())
}

pub(crate) fn plan(info: &MediaInfo, config: &ConversionConfig) -> Result<Plan, EngineError> {
    validate_fields(config)?;
    validate_operation(info, config.operation)?;
    let muxer_name = CString::new(config.format.muxer()).unwrap();
    let muxer =
        unsafe { sys::av_guess_format(muxer_name.as_ptr(), std::ptr::null(), std::ptr::null()) };
    if muxer.is_null() {
        return Err(invalid(
            "The selected output format is unavailable in this build.",
        ));
    }
    let mut streams = Vec::new();
    for stream in &info.streams {
        let mut item = StreamPlan {
            index: stream.index,
            encoder: None,
            expected_codec: stream.codec.clone(),
            width: None,
            height: None,
            sample_rate: None,
            channels: None,
            bit_depth: None,
        };
        if stream.kind == StreamKind::Video
            && !stream.video.as_ref().is_some_and(|v| v.attached_picture)
        {
            let video = stream
                .video
                .as_ref()
                .ok_or_else(|| invalid("Video geometry could not be inspected."))?;
            let transform = config.video_size != VideoSize::Original
                || config.frame_rate != FrameRate::Original
                || config.quality != 23;
            let copy_compatible =
                unsafe { sys::avformat_query_codec(muxer, stream.codec_id as _, 0) } > 0;
            let encode = config.video_codec == VideoCodec::H264
                || (config.video_codec == VideoCodec::Automatic && (transform || !copy_compatible));
            if encode {
                if !stream.decoder_available {
                    return Err(invalid(
                        "This source requires a video decoder that is unavailable.",
                    ));
                }
                if video.bit_depth.is_some_and(|depth| depth > 8)
                    || video.mastering_display.is_some()
                    || video.content_light.is_some()
                    || video
                        .color_transfer
                        .as_deref()
                        .is_some_and(|v| matches!(v, "smpte2084" | "arib-std-b67"))
                {
                    return Err(invalid("The current H.264 encoding route cannot preserve this source's high bit depth or HDR. Use compatible stream copying; HDR conversion will be added later."));
                }
                let width = video
                    .width
                    .ok_or_else(|| invalid("The source width is unknown."))?;
                let height = video
                    .height
                    .ok_or_else(|| invalid("The source height is unknown."))?;
                if width == 0 || height == 0 || width > 16384 || height > 16384 {
                    return Err(invalid(
                        "The source geometry exceeds this processing route's limits.",
                    ));
                }
                let sar = video
                    .sample_aspect_ratio
                    .as_ref()
                    .map(|r| f64::from(r.numerator) / f64::from(r.denominator))
                    .unwrap_or(1.0);
                if !sar.is_finite() || sar <= 0.0 {
                    return Err(invalid("The source pixel aspect ratio is invalid."));
                }
                let factor = match config.video_size {
                    VideoSize::Original => 1.0,
                    size => {
                        let long = if size == VideoSize::Hd720 {
                            1280.0
                        } else {
                            854.0
                        };
                        let short = if size == VideoSize::Hd720 {
                            720.0
                        } else {
                            480.0
                        };
                        let dw = f64::from(width) * sar;
                        let dh = f64::from(height);
                        let (bw, bh) = if dw >= dh {
                            (long, short)
                        } else {
                            (short, long)
                        };
                        (bw / dw).min(bh / dh).min(1.0)
                    }
                };
                let (out_w, out_h) = if factor < 1.0 {
                    (
                        ((f64::from(width) * factor) as u32 / 2) * 2,
                        ((f64::from(height) * factor) as u32 / 2) * 2,
                    )
                } else {
                    (width, height)
                };
                if out_w < 2 || out_h < 2 || out_w % 2 != 0 || out_h % 2 != 0 {
                    return Err(invalid("H.264 requires even dimensions. Choose a supported size or compatible stream copying."));
                }
                item.encoder = Some("libx264".into());
                item.expected_codec = "h264".into();
                item.width = Some(out_w);
                item.height = Some(out_h);
            }
        } else if stream.kind == StreamKind::Audio {
            let audio = stream
                .audio
                .as_ref()
                .ok_or_else(|| invalid("Audio properties could not be inspected."))?;
            let desired = if config.operation == Operation::VideoConverter {
                "aac"
            } else {
                match config.format {
                    OutputFormat::M4a => "aac",
                    OutputFormat::Mp3 => "mp3",
                    OutputFormat::Wav => "pcm_s16le",
                    OutputFormat::Flac => "flac",
                    _ => unreachable!(),
                }
            };
            let encoder = if desired == "mp3" {
                "libmp3lame"
            } else {
                desired
            };
            let transform = config.sample_rate.is_some()
                || config.channels.is_some()
                || config.audio_bitrate != 192_000;
            let compatible =
                unsafe { sys::avformat_query_codec(muxer, stream.codec_id as _, 0) } > 0;
            let encode = config.audio_codec != AudioCodec::Copy
                && (config.audio_codec != AudioCodec::Automatic
                    || transform
                    || !compatible
                    || (config.operation == Operation::AudioConverter && stream.codec != desired));
            if encode {
                if !stream.decoder_available {
                    return Err(invalid(
                        "This source requires an audio decoder that is unavailable.",
                    ));
                }
                let rate = config
                    .sample_rate
                    .or(audio.sample_rate)
                    .ok_or_else(|| invalid("The source sample rate is unknown."))?;
                let channels = config
                    .channels
                    .or(audio.channels)
                    .ok_or_else(|| invalid("The source channel layout is unknown."))?;
                if channels == 0 || channels > 8 || rate < 8000 || rate > 192000 {
                    return Err(invalid("The selected sampling or channel layout is outside this route's supported range."));
                }
                if desired == "aac"
                    && ![
                        8000, 11025, 12000, 16000, 22050, 24000, 32000, 44100, 48000, 64000, 88200,
                        96000,
                    ]
                    .contains(&rate)
                {
                    return Err(invalid(
                        "AAC cannot use this sample rate. Select 44.1 or 48 kHz.",
                    ));
                }
                if desired == "mp3"
                    && (channels > 2
                        || ![8000, 11025, 12000, 16000, 22050, 24000, 32000, 44100, 48000]
                            .contains(&rate))
                {
                    return Err(invalid("MP3 requires a supported sample rate and Mono or Stereo channels. Choose explicit conversion settings."));
                }
                if desired == "flac" {
                    if audio.bit_depth.is_some_and(|depth| depth > 24) {
                        return Err(invalid("This FLAC encoding route preserves up to 24-bit integer precision. The source requires greater precision; choose another compatible representation."));
                    }
                    // A decoded lossy source may not have a meaningful coded
                    // precision. Use 24-bit FLAC instead of silently choosing 16.
                    item.bit_depth = Some(audio.bit_depth.unwrap_or(24));
                }
                item.encoder = Some(encoder.into());
                item.expected_codec = desired.into();
                item.sample_rate = Some(rate);
                item.channels = Some(channels);
            }
        }
        let target_id = if let Some(encoder) = &item.encoder {
            codec_id(encoder)?
        } else {
            stream.codec_id
        };
        if unsafe { sys::avformat_query_codec(muxer, target_id as _, 0) } <= 0 {
            return Err(invalid(format!("{} stream {} is incompatible with this container. Choose a compatible format; tracks are never silently discarded.",stream.codec,stream.index)));
        }
        streams.push(item);
    }
    Ok(Plan {
        config: config.clone(),
        streams,
    })
}

pub(crate) fn verify_output(
    info: &MediaInfo,
    source: &MediaInfo,
    plan: &Plan,
) -> Result<(), EngineError> {
    if info.streams.len() != plan.streams.len() {
        return Err(EngineError::InvalidMedia {
            detail: "The finalized output has an unexpected stream count.".into(),
        });
    }
    for (stream, expected) in info.streams.iter().zip(&plan.streams) {
        if stream.codec != expected.expected_codec {
            return Err(EngineError::InvalidMedia {
                detail: format!("Output stream {} has an unexpected codec.", stream.index),
            });
        }
        if let Some(width) = expected.width {
            let video = stream
                .video
                .as_ref()
                .ok_or_else(|| invalid("The output video stream is missing."))?;
            if video.width != Some(width) || video.height != expected.height {
                return Err(EngineError::InvalidMedia {
                    detail:
                        "The finalized video geometry does not match the accepted configuration."
                            .into(),
                });
            }
        }
        if let Some(rate) = expected.sample_rate {
            let audio = stream
                .audio
                .as_ref()
                .ok_or_else(|| invalid("The output audio stream is missing."))?;
            if expected.bit_depth.is_some() && audio.bit_depth != expected.bit_depth {
                return Err(EngineError::InvalidMedia {
                    detail:
                        "The finalized audio precision does not match the accepted configuration."
                            .into(),
                });
            }
            if audio.sample_rate != Some(rate) || audio.channels != expected.channels {
                return Err(EngineError::InvalidMedia {
                    detail:
                        "The finalized audio sampling does not match the accepted configuration."
                            .into(),
                });
            }
        }
    }
    if let Some(expected) = source.duration_us.filter(|value| *value > 0) {
        let actual = info.duration_us.ok_or_else(|| EngineError::InvalidMedia {
            detail: "The finalized output has no verifiable duration.".into(),
        })?;
        if actual <= 0 || actual.abs_diff(expected) > 500_000u64.max(expected as u64 / 100) {
            return Err(EngineError::InvalidMedia {
                detail: "The finalized duration differs unexpectedly from the source.".into(),
            });
        }
    }
    Ok(())
}
