//! Cheap video output policy shared by acceptance and workspace projections.
//! This does not open a source/encoder, build a pipeline or mutate user intent.
use super::{
    apply_compression, compression_copy_policy, configured_encoding_dimensions,
    configured_video_dimensions, invalid, muxer_accepts, scaled_video_dimensions,
    video_encoding_settings, CompressionCopyPolicy, CompressionMedia, CompressionMode,
    ConversionConfig, FrameRate, OutputFormat, VideoCodec, VideoSize,
};
use crate::{processing::encoding::EncodingSettings, EngineError, MediaInfo, Rational, StreamInfo};

pub(crate) struct VideoOutput {
    pub width: u32,
    pub height: u32,
    pub frame_rate: Option<Rational>,
    pub settings: EncodingSettings,
    pub transform: Option<super::VideoTransformPlan>,
}

pub(crate) fn video_codec(name: &str) -> Result<VideoCodec, EngineError> {
    [
        VideoCodec::H264,
        VideoCodec::Hevc,
        VideoCodec::Av1,
        VideoCodec::Vp8,
        VideoCodec::Vp9,
        VideoCodec::Mpeg4,
        VideoCodec::Ffv1,
        VideoCodec::Mjpeg,
    ]
    .into_iter()
    .find(|codec| codec.codec_name() == Some(name))
    .ok_or_else(|| invalid("Choose an implemented video encoder for compression."))
}

/// Apply explicitly authorized optimization to a temporary projection. The
/// accepted configuration remains independent of these effective dimensions.
pub(crate) fn effective_config(
    info: &MediaInfo,
    config: &ConversionConfig,
) -> Result<ConversionConfig, EngineError> {
    let mut effective = config.clone();
    let Some(compression) = config
        .compression
        .as_ref()
        .filter(|settings| settings.reduce_file_size)
    else {
        return Ok(effective);
    };
    if compression.optimize_resolution {
        effective.video_size = VideoSize::P1080;
    }
    if compression.optimize_frame_rate {
        let source = info
            .streams
            .iter()
            .filter_map(|stream| stream.video.as_ref())
            .find(|video| !video.attached_picture)
            .ok_or_else(|| invalid("Frame-rate optimization requires a video source."))?;
        let rate = source.nominal_frame_rate.as_ref().or(source.average_frame_rate.as_ref())
            .filter(|rate| rate.numerator > 0 && rate.denominator > 0)
            .ok_or_else(|| invalid("The source rate is unknown. Choose an explicit frame rate instead of automatic optimization."))?;
        if i64::from(rate.numerator) > 30 * i64::from(rate.denominator) {
            effective.frame_rate = FrameRate::Fps30;
        }
    }
    Ok(effective)
}

fn force_video(
    stream: &StreamInfo,
    format: OutputFormat,
    config: &ConversionConfig,
) -> Result<bool, EngineError> {
    let Some(compression) = config
        .compression
        .as_ref()
        .filter(|settings| settings.reduce_file_size)
    else {
        return Ok(false);
    };
    if compression.mode == CompressionMode::Custom && compression.custom_video.is_none() {
        return Ok(false);
    }
    if config.video_codec == VideoCodec::Copy {
        if compression.mode == CompressionMode::TargetSize
            || stream.codec == "ffv1" && compression.mode != CompressionMode::Custom
        {
            return Ok(false);
        }
        return Err(invalid("Stream copying cannot apply video compression controls. Choose Automatic or an applicable encoder."));
    }
    if config.video_codec == VideoCodec::Automatic
        && stream.codec == "ffv1"
        && compression.mode != CompressionMode::Custom
    {
        return Ok(false);
    }
    let facts = stream
        .video
        .as_ref()
        .ok_or_else(|| invalid("The video properties are unknown."))?;
    let profile = video_encoding_settings(
        format,
        config.video_codec,
        facts,
        &stream.codec,
        config.quality,
    )?;
    let kind = CompressionMedia::Video {
        codec: video_codec(&profile.codec)?,
    };
    let policy = if compression.mode == CompressionMode::TargetSize {
        compression_copy_policy(kind, compression)?
    } else {
        apply_compression(&profile, kind, compression, None)?.copy_policy
    };
    Ok(matches!(
        policy,
        CompressionCopyPolicy::EncodeLossy | CompressionCopyPolicy::ReencodeLosslessEffort
    ))
}

pub(crate) fn resolve_video_output(
    stream: &StreamInfo,
    format: OutputFormat,
    config: &ConversionConfig,
) -> Result<Option<VideoOutput>, EngineError> {
    let video = stream
        .video
        .as_ref()
        .ok_or_else(|| invalid("Video geometry could not be inspected."))?;
    let size_change = config.video_size != VideoSize::Original
        && configured_video_dimensions(video, config)?
            != scaled_video_dimensions(video, VideoSize::Original)?;
    let transform = size_change
        || config.frame_rate != FrameRate::Original
        || config.quality != 23
        || config
            .video_adjustments
            .as_ref()
            .is_some_and(|settings| settings.active());
    let compression_encode = force_video(stream, format, config)?;
    if config.video_codec == VideoCodec::Copy && transform {
        return Err(invalid(
            "These video settings require encoding. Choose an applicable codec or Automatic.",
        ));
    }
    let compatible = muxer_accepts(format, stream.codec_id);
    let encode = !matches!(config.video_codec, VideoCodec::Automatic | VideoCodec::Copy)
        || config.video_codec == VideoCodec::Automatic
            && (transform || !compatible || compression_encode);
    if !encode {
        return Ok(None);
    }
    if !stream.decoder_available {
        return Err(invalid(
            "This source requires an unavailable video decoder.",
        ));
    }
    let settings = video_encoding_settings(
        format,
        config.video_codec,
        video,
        &stream.codec,
        config.quality,
    )?;
    let dimensions = configured_encoding_dimensions(
        video,
        config,
        settings
            .pixel_format
            .as_deref()
            .ok_or_else(|| invalid("The encoder pixel representation is missing."))?,
    )?;
    Ok(Some(VideoOutput {
        transform: super::spatial::transform_plan(video, config, dimensions.0, dimensions.1)?,
        width: dimensions.0,
        height: dimensions.1,
        frame_rate: config
            .frame_rate
            .rational(config.custom_frame_rate.as_ref())?,
        settings,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn compatible_copy_projection_needs_no_decoder_but_a_real_transform_does() {
        let source: StreamInfo = serde_json::from_value(json!({
            "id":"source:0:0","index":0,"container_id":0,"kind":"Video",
            "codec_id":crate::ffmpeg::sys::AV_CODEC_ID_H264,"codec":"h264",
            "decoder_available":false,"duration_us":2000000,"start_time_us":0,
            "disposition":0,"metadata":[],
            "video":{"width":320,"height":180,"display_matrix":[],"attached_picture":false}
        }))
        .unwrap();
        let mut config: ConversionConfig = serde_json::from_value(json!({
            "operation":"VideoConverter","format":"Mp4","video_codec":"Automatic",
            "audio_codec":"Automatic","video_size":"Original","frame_rate":"Original",
            "quality":23,"file_name":"video"
        }))
        .unwrap();
        assert!(resolve_video_output(&source, OutputFormat::Mp4, &config)
            .unwrap()
            .is_none());
        config.frame_rate = FrameRate::Fps30;
        assert!(matches!(
            resolve_video_output(&source, OutputFormat::Mp4, &config),
            Err(EngineError::Configuration { .. })
        ));
        config.video_codec = VideoCodec::Copy;
        assert!(matches!(
            resolve_video_output(&source, OutputFormat::Mp4, &config),
            Err(EngineError::Configuration { .. })
        ));
    }
}
