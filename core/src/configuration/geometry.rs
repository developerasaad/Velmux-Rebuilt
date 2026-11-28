use super::{invalid, ConversionConfig};
use crate::{EngineError, Rational, VideoInfo};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, uniffi::Enum)]
pub enum VideoSize {
    Original,
    P2160,
    P1440,
    P1080,
    Hd720,
    Sd480,
    P360,
    Custom,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, uniffi::Enum)]
pub enum FrameRate {
    Original,
    Fps23976,
    Fps24,
    Fps25,
    Fps2997,
    Fps30,
    Fps48,
    Fps50,
    Fps5994,
    Fps60,
    Custom,
}

pub(crate) const VIDEO_SIZES: [VideoSize; 8] = [
    VideoSize::Original,
    VideoSize::P2160,
    VideoSize::P1440,
    VideoSize::P1080,
    VideoSize::Hd720,
    VideoSize::Sd480,
    VideoSize::P360,
    VideoSize::Custom,
];
pub(crate) const FRAME_RATES: [FrameRate; 11] = [
    FrameRate::Original,
    FrameRate::Fps23976,
    FrameRate::Fps24,
    FrameRate::Fps25,
    FrameRate::Fps2997,
    FrameRate::Fps30,
    FrameRate::Fps48,
    FrameRate::Fps50,
    FrameRate::Fps5994,
    FrameRate::Fps60,
    FrameRate::Custom,
];

impl FrameRate {
    pub(crate) fn rational(
        self,
        custom: Option<&Rational>,
    ) -> Result<Option<Rational>, EngineError> {
        let (numerator, denominator) = match self {
            Self::Original => return Ok(None),
            Self::Fps23976 => (24000, 1001),
            Self::Fps24 => (24, 1),
            Self::Fps25 => (25, 1),
            Self::Fps2997 => (30000, 1001),
            Self::Fps30 => (30, 1),
            Self::Fps48 => (48, 1),
            Self::Fps50 => (50, 1),
            Self::Fps5994 => (60000, 1001),
            Self::Fps60 => (60, 1),
            Self::Custom => {
                let value = custom.ok_or_else(|| {
                    invalid("Enter a custom frame-rate numerator and denominator.")
                })?;
                (value.numerator, value.denominator)
            }
        };
        if numerator <= 0
            || denominator <= 0
            || numerator > 240_000
            || denominator > 100_000
            || i64::from(numerator) > i64::from(denominator) * 240
            || i64::from(numerator) * 100 < i64::from(denominator)
        {
            return Err(invalid("Use a frame rate from 0.01 to 240 fps with a positive numerator up to 240000 and denominator up to 100000."));
        }
        let mut a = numerator;
        let mut b = denominator;
        while b != 0 {
            (a, b) = (b, a % b);
        }
        Ok(Some(Rational {
            numerator: numerator / a,
            denominator: denominator / a,
        }))
    }
}

fn display_geometry(video: &VideoInfo) -> Result<(f64, f64), EngineError> {
    let width = video
        .width
        .filter(|value| *value > 0)
        .ok_or_else(|| invalid("The source width is unknown."))?;
    let height = video
        .height
        .filter(|value| *value > 0)
        .ok_or_else(|| invalid("The source height is unknown."))?;
    let sar = video.sample_aspect_ratio.as_ref().map_or(1.0, |ratio| {
        f64::from(ratio.numerator) / f64::from(ratio.denominator)
    });
    if !sar.is_finite() || sar <= 0.0 {
        return Err(invalid("The source pixel aspect ratio is invalid."));
    }
    let (width, height) = (f64::from(width) * sar, f64::from(height));
    let rotation = video.rotation_degrees_counter_clockwise.unwrap_or(0.0);
    if !rotation.is_finite() {
        return Err(invalid("The source orientation is invalid."));
    }
    let quarter = rotation / 90.0;
    if (quarter - quarter.round()).abs() < 0.00001 {
        return Ok(if (quarter.round() as i64).rem_euclid(2) == 0 {
            (width, height)
        } else {
            (height, width)
        });
    }
    let angle = rotation.to_radians();
    Ok((
        width * angle.cos().abs() + height * angle.sin().abs(),
        width * angle.sin().abs() + height * angle.cos().abs(),
    ))
}

pub(crate) fn custom_geometry_defaults(video: Option<&VideoInfo>) -> (u32, u32, Rational) {
    let dimensions = video
        .and_then(|video| display_geometry(video).ok())
        .unwrap_or((1280.0, 720.0));
    let width = (dimensions.0.round() as u32).clamp(2, 16384);
    let height = (dimensions.1.round() as u32).clamp(2, 16384);
    let rate = video
        .and_then(|video| video.average_frame_rate.clone())
        .filter(|rate| FrameRate::Custom.rational(Some(rate)).is_ok())
        .unwrap_or(Rational {
            numerator: 30,
            denominator: 1,
        });
    (width, height, rate)
}

pub(crate) fn scaled_video_dimensions(
    video: &VideoInfo,
    size: VideoSize,
) -> Result<(u32, u32), EngineError> {
    video_dimensions(video, size, None, None)
}

pub(crate) fn configured_video_dimensions(
    video: &VideoInfo,
    config: &ConversionConfig,
) -> Result<(u32, u32), EngineError> {
    video_dimensions(
        video,
        config.video_size,
        config.custom_width,
        config.custom_height,
    )
}

fn video_dimensions(
    video: &VideoInfo,
    size: VideoSize,
    custom_width: Option<u32>,
    custom_height: Option<u32>,
) -> Result<(u32, u32), EngineError> {
    let width = video
        .width
        .filter(|value| *value > 0)
        .ok_or_else(|| invalid("The source width is unknown."))?;
    let height = video
        .height
        .filter(|value| *value > 0)
        .ok_or_else(|| invalid("The source height is unknown."))?;
    if size == VideoSize::Original {
        return Ok((width, height));
    }
    let (dw, dh) = display_geometry(video)?;
    let (bw, bh) = match size {
        VideoSize::Custom => {
            let (w, h) = custom_width
                .zip(custom_height)
                .ok_or_else(|| invalid("Enter both custom resolution bounds."))?;
            if !(2..=16384).contains(&w)
                || !(2..=16384).contains(&h)
                || u64::from(w) * u64::from(h) > 64 * 1024 * 1024
            {
                return Err(invalid(
                    "Custom bounds must be 2–16384 pixels per side and at most 64 megapixels.",
                ));
            }
            (f64::from(w), f64::from(h))
        }
        _ => {
            let (long, short) = match size {
                VideoSize::P2160 => (3840.0, 2160.0),
                VideoSize::P1440 => (2560.0, 1440.0),
                VideoSize::P1080 => (1920.0, 1080.0),
                VideoSize::Hd720 => (1280.0, 720.0),
                VideoSize::Sd480 => (854.0, 480.0),
                VideoSize::P360 => (640.0, 360.0),
                _ => unreachable!(),
            };
            if dw >= dh {
                (long, short)
            } else {
                (short, long)
            }
        }
    };
    let factor = (bw / dw).min(bh / dh);
    let factor = if size == VideoSize::Custom {
        factor
    } else {
        factor.min(1.0)
    };
    if size != VideoSize::Custom && factor >= 1.0 {
        return Ok((width, height));
    }
    let out_w = ((f64::from(width) * factor).floor() as u32 / 2) * 2;
    let out_h = ((f64::from(height) * factor).floor() as u32 / 2) * 2;
    if out_w < 2
        || out_h < 2
        || out_w > 16384
        || out_h > 16384
        || u64::from(out_w) * u64::from(out_h) > 64 * 1024 * 1024
    {
        return Err(invalid("These bounds cannot produce valid aligned video dimensions within the processing limits."));
    }
    Ok((out_w, out_h))
}

pub(crate) fn h264_dimensions(
    video: &VideoInfo,
    size: VideoSize,
) -> Result<(u32, u32), EngineError> {
    validate_h264(video, scaled_video_dimensions(video, size)?)
}
pub(crate) fn configured_h264_dimensions(
    video: &VideoInfo,
    config: &ConversionConfig,
) -> Result<(u32, u32), EngineError> {
    validate_h264(video, configured_video_dimensions(video, config)?)
}
fn validate_h264(video: &VideoInfo, dimensions: (u32, u32)) -> Result<(u32, u32), EngineError> {
    if video.width.is_some_and(|w| w > 16384)
        || video.height.is_some_and(|h| h > 16384)
        || video
            .width
            .zip(video.height)
            .is_some_and(|(w, h)| u64::from(w) * u64::from(h) > 64 * 1024 * 1024)
    {
        return Err(invalid(
            "The source geometry exceeds this processing route's limits.",
        ));
    }
    if dimensions.0 < 2
        || dimensions.1 < 2
        || !dimensions.0.is_multiple_of(2)
        || !dimensions.1.is_multiple_of(2)
    {
        return Err(invalid("H.264 requires even dimensions. Choose explicit custom bounds or compatible stream copying."));
    }
    Ok(dimensions)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn video(width: u32, height: u32) -> VideoInfo {
        serde_json::from_value(
            json!({"width":width,"height":height,"display_matrix":[],"attached_picture":false}),
        )
        .unwrap()
    }

    #[test]
    fn bounds_preserve_display_orientation_and_never_automatically_upscale() {
        let portrait = video(1080, 1920);
        assert_eq!(
            scaled_video_dimensions(&portrait, VideoSize::Hd720).unwrap(),
            (720, 1280)
        );
        let mut rotated = video(1920, 1080);
        rotated.rotation_degrees_counter_clockwise = Some(90.0);
        assert_eq!(
            scaled_video_dimensions(&rotated, VideoSize::Hd720).unwrap(),
            (1280, 720)
        );
        for size in VIDEO_SIZES
            .into_iter()
            .filter(|size| *size != VideoSize::Custom)
        {
            assert_eq!(
                scaled_video_dimensions(&video(180, 320), size).unwrap(),
                (180, 320)
            );
        }
        assert_eq!(
            video_dimensions(&video(320, 180), VideoSize::Custom, Some(640), Some(360)).unwrap(),
            (640, 360)
        );
        assert_eq!(
            video_dimensions(&video(180, 320), VideoSize::Custom, Some(320), Some(300)).unwrap(),
            (168, 300)
        );
    }

    #[test]
    fn every_preset_has_its_actual_bound_and_custom_limits_are_checked() {
        let landscape = video(7680, 4320);
        for (size, expected) in [
            (VideoSize::P2160, (3840, 2160)),
            (VideoSize::P1440, (2560, 1440)),
            (VideoSize::P1080, (1920, 1080)),
            (VideoSize::Hd720, (1280, 720)),
            (VideoSize::Sd480, (852, 480)),
            (VideoSize::P360, (640, 360)),
        ] {
            assert_eq!(scaled_video_dimensions(&landscape, size).unwrap(), expected);
        }
        assert!(
            video_dimensions(&landscape, VideoSize::Custom, Some(u32::MAX), Some(720)).is_err()
        );
        assert!(video_dimensions(&landscape, VideoSize::Custom, Some(1280), None).is_err());
        assert!(video_dimensions(&landscape, VideoSize::Custom, Some(2), Some(16384)).is_err());
    }

    #[test]
    fn fractional_and_custom_rates_use_reduced_exact_rationals() {
        for (rate, num) in [
            (FrameRate::Fps23976, 24000),
            (FrameRate::Fps2997, 30000),
            (FrameRate::Fps5994, 60000),
        ] {
            assert_eq!(
                rate.rational(None).unwrap(),
                Some(Rational {
                    numerator: num,
                    denominator: 1001
                })
            );
        }
        assert_eq!(
            FrameRate::Custom
                .rational(Some(&Rational {
                    numerator: 48000,
                    denominator: 2002
                }))
                .unwrap(),
            Some(Rational {
                numerator: 24000,
                denominator: 1001
            })
        );
        for (numerator, denominator) in [(0, 1), (30, 0), (241, 1), (i32::MAX, 1), (1, 1000)] {
            assert!(FrameRate::Custom
                .rational(Some(&Rational {
                    numerator,
                    denominator
                }))
                .is_err());
        }
    }
}
