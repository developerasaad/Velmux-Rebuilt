//! Quarter-turn and reflection policy over immutable inspected source facts.
use super::{invalid, ConversionConfig};
use crate::{ffmpeg::sys, EngineError, Rational, VideoInfo};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, uniffi::Enum)]
pub enum VideoRotation {
    #[default]
    Zero,
    Clockwise90,
    Clockwise180,
    Clockwise270,
}
impl VideoRotation {
    fn quarters(self) -> u8 {
        match self {
            Self::Zero => 0,
            Self::Clockwise90 => 1,
            Self::Clockwise180 => 2,
            Self::Clockwise270 => 3,
        }
    }
}

/// Additional presentation-space adjustments, after source display orientation.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, uniffi::Record)]
#[serde(default)]
pub struct VideoAdjustments {
    pub rotation: VideoRotation,
    pub horizontal_flip: bool,
    pub vertical_flip: bool,
}
impl VideoAdjustments {
    pub(crate) fn active(&self) -> bool {
        self.rotation != VideoRotation::Zero || self.horizontal_flip || self.vertical_flip
    }
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct VideoAdjustmentLimits {
    pub rotations: Vec<VideoRotation>,
    pub can_flip_horizontal: bool,
    pub can_flip_vertical: bool,
}

/// Some means the complete presentation orientation is physically baked and
/// must no longer be repeated by stream or decoded-frame display metadata.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct VideoTransformPlan {
    pub clockwise_quarter_turns: u8,
    pub flip_horizontal: bool,
    pub output_sample_aspect_ratio: Rational,
    pub intermediate_pixel_format: Option<String>,
}
impl VideoTransformPlan {
    pub(crate) fn filters(&self) -> Result<String, EngineError> {
        let mut chain = String::new();
        if let Some(pixel) = &self.intermediate_pixel_format {
            // The name comes from the core's verified scalar pixel catalog.
            let name = std::ffi::CString::new(pixel.as_str())
                .map_err(|_| invalid("The accepted spatial pixel format is invalid."))?;
            if unsafe { sys::av_get_pix_fmt(name.as_ptr()) } == sys::AV_PIX_FMT_NONE {
                return Err(invalid(
                    "The accepted spatial pixel representation is unavailable.",
                ));
            }
            chain.push_str(&format!("format=pix_fmts={pixel},"));
        }
        let needs_transpose = matches!(self.clockwise_quarter_turns, 1 | 3);
        for (needed, name) in [
            (needs_transpose, c"transpose"),
            (
                self.clockwise_quarter_turns == 2 || self.flip_horizontal,
                c"hflip",
            ),
            (self.clockwise_quarter_turns == 2, c"vflip"),
        ] {
            if needed && unsafe { sys::avfilter_get_by_name(name.as_ptr()).is_null() } {
                return Err(invalid(
                    "A required spatial video filter is unavailable in this build.",
                ));
            }
        }
        match self.clockwise_quarter_turns {
            0 => {}
            1 => chain.push_str("transpose=clock,"),
            2 => chain.push_str("hflip,vflip,"),
            3 => chain.push_str("transpose=cclock,"),
            _ => return Err(invalid("The accepted video orientation is invalid.")),
        }
        if self.flip_horizontal {
            chain.push_str("hflip,");
        }
        Ok(chain)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Orientation {
    xx: i32,
    xy: i32,
    yx: i32,
    yy: i32,
}
impl Orientation {
    const ID: Self = Self {
        xx: 1,
        xy: 0,
        yx: 0,
        yy: 1,
    };
    const H: Self = Self {
        xx: -1,
        xy: 0,
        yx: 0,
        yy: 1,
    };
    const V: Self = Self {
        xx: 1,
        xy: 0,
        yx: 0,
        yy: -1,
    };
    fn rotation(q: u8) -> Self {
        match q % 4 {
            0 => Self::ID,
            1 => Self {
                xx: 0,
                xy: -1,
                yx: 1,
                yy: 0,
            },
            2 => Self {
                xx: -1,
                xy: 0,
                yx: 0,
                yy: -1,
            },
            _ => Self {
                xx: 0,
                xy: 1,
                yx: -1,
                yy: 0,
            },
        }
    }
    fn then(self, next: Self) -> Self {
        Self {
            xx: next.xx * self.xx + next.xy * self.yx,
            xy: next.xx * self.xy + next.xy * self.yy,
            yx: next.yx * self.xx + next.yy * self.yx,
            yy: next.yx * self.xy + next.yy * self.yy,
        }
    }
    fn swaps(self) -> bool {
        self.xx == 0
    }
    fn canonical(self) -> (u8, bool) {
        for q in 0..4 {
            for flip in [false, true] {
                let candidate = if flip {
                    Self::rotation(q).then(Self::H)
                } else {
                    Self::rotation(q)
                };
                if candidate == self {
                    return (q, flip);
                }
            }
        }
        unreachable!("validated orthogonal orientations form the dihedral group")
    }
}
fn unsupported() -> EngineError {
    invalid("This source has scaling, shear, perspective or an arbitrary display orientation that this quarter-turn route cannot bake safely. Keep adjustments off to preserve compatible orientation, or select another source.")
}
fn source_orientation(video: &VideoInfo) -> Result<Orientation, EngineError> {
    if video.display_matrix.is_empty() {
        return if video
            .rotation_degrees_counter_clockwise
            .is_none_or(|angle| angle.is_finite() && angle.abs() < 0.00001)
        {
            Ok(Orientation::ID)
        } else {
            Err(unsupported())
        };
    }
    let m = &video.display_matrix;
    if m.len() != 9 || m[2] != 0 || m[5] != 0 || m[8] != 1 << 30 {
        return Err(unsupported());
    }
    let unit = |value: i32| -> Result<i32, EngineError> {
        [-1, 0, 1]
            .into_iter()
            .find(|candidate| (i64::from(value) - i64::from(*candidate) * 65536).abs() <= 1)
            .ok_or_else(unsupported)
    };
    let orientation = Orientation {
        xx: unit(m[0])?,
        xy: unit(m[3])?,
        yx: unit(m[1])?,
        yy: unit(m[4])?,
    };
    if orientation.xx * orientation.xx + orientation.xy * orientation.xy != 1
        || orientation.yx * orientation.yx + orientation.yy * orientation.yy != 1
        || orientation.xx * orientation.yx + orientation.xy * orientation.yy != 0
    {
        return Err(unsupported());
    }
    let (w, h) = video
        .width
        .zip(video.height)
        .filter(|(w, h)| *w > 0 && *h > 0)
        .ok_or_else(unsupported)?;
    let translation = |w: i64, h: i64| {
        let x = [
            0,
            i64::from(orientation.xx) * w,
            i64::from(orientation.xy) * h,
            i64::from(orientation.xx) * w + i64::from(orientation.xy) * h,
        ];
        let y = [
            0,
            i64::from(orientation.yx) * w,
            i64::from(orientation.yy) * h,
            i64::from(orientation.yx) * w + i64::from(orientation.yy) * h,
        ];
        (
            -x.into_iter().min().unwrap() * 65536,
            -y.into_iter().min().unwrap() * 65536,
        )
    };
    // Containers use either edge or pixel-center bounds for canonical rebasing;
    // pure FFmpeg orientation matrices also legitimately omit translation.
    if ![
        (0, 0),
        translation(i64::from(w), i64::from(h)),
        translation(i64::from(w) - 1, i64::from(h) - 1),
    ]
    .into_iter()
    .any(|(x, y)| (i64::from(m[6]) - x).abs() <= 1 && (i64::from(m[7]) - y).abs() <= 1)
    {
        return Err(unsupported());
    }
    Ok(orientation)
}

pub(crate) fn limits(video: &VideoInfo) -> Option<VideoAdjustmentLimits> {
    source_orientation(video).ok()?;
    let available =
        |name: &std::ffi::CStr| !unsafe { sys::avfilter_get_by_name(name.as_ptr()).is_null() };
    let h = available(c"hflip");
    let v = available(c"vflip");
    let transpose = available(c"transpose");
    let mut rotations = vec![VideoRotation::Zero];
    if transpose {
        rotations.push(VideoRotation::Clockwise90);
    }
    if h && v {
        rotations.push(VideoRotation::Clockwise180);
    }
    if transpose {
        rotations.push(VideoRotation::Clockwise270);
    }
    Some(VideoAdjustmentLimits {
        rotations,
        can_flip_horizontal: h,
        can_flip_vertical: v,
    })
}
fn composed(
    video: &VideoInfo,
    config: &ConversionConfig,
) -> Result<Option<Orientation>, EngineError> {
    let Some(settings) = config
        .video_adjustments
        .as_ref()
        .filter(|settings| settings.active())
    else {
        return Ok(None);
    };
    let available = limits(video).ok_or_else(unsupported)?;
    if !available.rotations.contains(&settings.rotation)
        || settings.horizontal_flip && !available.can_flip_horizontal
        || settings.vertical_flip && !available.can_flip_vertical
    {
        return Err(invalid(
            "A required spatial video filter is unavailable in this build.",
        ));
    }
    let mut orientation =
        source_orientation(video)?.then(Orientation::rotation(settings.rotation.quarters()));
    if settings.horizontal_flip {
        orientation = orientation.then(Orientation::H);
    }
    if settings.vertical_flip {
        orientation = orientation.then(Orientation::V);
    }
    Ok(Some(orientation))
}

pub(crate) fn ratio(numerator: i128, denominator: i128) -> Result<Rational, EngineError> {
    if numerator <= 0 || denominator <= 0 {
        return Err(invalid("The video sample aspect ratio is invalid."));
    }
    let (mut a, mut b) = (numerator, denominator);
    while b != 0 {
        (a, b) = (b, a % b);
    }
    Ok(Rational {
        numerator: i32::try_from(numerator / a)
            .map_err(|_| invalid("The video aspect ratio exceeds supported bounds."))?,
        denominator: i32::try_from(denominator / a)
            .map_err(|_| invalid("The video aspect ratio exceeds supported bounds."))?,
    })
}

pub(crate) fn oriented_video(
    video: &VideoInfo,
    config: &ConversionConfig,
) -> Result<VideoInfo, EngineError> {
    let Some(orientation) = composed(video, config)? else {
        return Ok(video.clone());
    };
    let mut result = video.clone();
    let sar = video.sample_aspect_ratio.clone().unwrap_or(Rational {
        numerator: 1,
        denominator: 1,
    });
    let sar = ratio(i128::from(sar.numerator), i128::from(sar.denominator))?;
    result.sample_aspect_ratio = Some(if orientation.swaps() {
        ratio(i128::from(sar.denominator), i128::from(sar.numerator))?
    } else {
        sar
    });
    if orientation.swaps() {
        std::mem::swap(&mut result.width, &mut result.height);
    }
    let (width, height) = result.width.zip(result.height).ok_or_else(unsupported)?;
    let sar = result.sample_aspect_ratio.as_ref().unwrap();
    result.display_aspect_ratio = Some(ratio(
        i128::from(width) * i128::from(sar.numerator),
        i128::from(height) * i128::from(sar.denominator),
    )?);
    result.display_matrix.clear();
    result.rotation_degrees_counter_clockwise = Some(0.0);
    Ok(result)
}

pub(crate) fn transform_plan(
    video: &VideoInfo,
    config: &ConversionConfig,
    width: u32,
    height: u32,
) -> Result<Option<VideoTransformPlan>, EngineError> {
    let Some(orientation) = composed(video, config)? else {
        return Ok(None);
    };
    let oriented = oriented_video(video, config)?;
    let (w, h) = oriented
        .width
        .zip(oriented.height)
        .ok_or_else(unsupported)?;
    let sar = oriented.sample_aspect_ratio.as_ref().unwrap();
    let output_sample_aspect_ratio = ratio(
        i128::from(sar.numerator) * i128::from(w) * i128::from(height),
        i128::from(sar.denominator) * i128::from(h) * i128::from(width),
    )?;
    let (clockwise_quarter_turns, flip_horizontal) = orientation.canonical();
    let intermediate_pixel_format = if orientation.swaps() {
        let source = std::ffi::CString::new(video.pixel_format.as_deref().ok_or_else(unsupported)?)
            .map_err(|_| unsupported())?;
        unsafe {
            let descriptor = sys::av_pix_fmt_desc_get(sys::av_get_pix_fmt(source.as_ptr()));
            if descriptor.is_null() {
                return Err(unsupported());
            }
            if (*descriptor).log2_chroma_w != (*descriptor).log2_chroma_h {
                let depth = (&(*descriptor).comp)
                    [..usize::from((*descriptor).nb_components).min(4)]
                    .iter()
                    .map(|component| component.depth)
                    .max()
                    .ok_or_else(unsupported)?;
                let alpha = (*descriptor).flags & u64::from(sys::AV_PIX_FMT_FLAG_ALPHA) != 0;
                let family = if alpha { "yuva444p" } else { "yuv444p" };
                let name = if depth == 8 {
                    family.into()
                } else {
                    format!("{family}{depth}le")
                };
                let name_c = std::ffi::CString::new(name.as_str()).map_err(|_| unsupported())?;
                if sys::av_get_pix_fmt(name_c.as_ptr()) == sys::AV_PIX_FMT_NONE {
                    return Err(unsupported());
                }
                Some(name)
            } else {
                None
            }
        }
    } else {
        None
    };
    let plan = VideoTransformPlan {
        clockwise_quarter_turns,
        flip_horizontal,
        output_sample_aspect_ratio,
        intermediate_pixel_format,
    };
    plan.filters()?;
    Ok(Some(plan))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn video() -> VideoInfo {
        serde_json::from_value(json!({"width":160,"height":96,"sample_aspect_ratio":{"numerator":4,"denominator":3},"pixel_format":"yuv420p","bit_depth":8,"attached_picture":false})).unwrap()
    }
    fn config() -> ConversionConfig {
        serde_json::from_value(json!({"operation":"VideoConverter","format":"Matroska","video_codec":"Ffv1","audio_codec":"Automatic","video_size":"Original","frame_rate":"Original","quality":23,"file_name":"spatial"})).unwrap()
    }
    fn matrix(o: Orientation, w: i32, h: i32) -> Vec<i32> {
        let tx = -[0, o.xx * w, o.xy * h, o.xx * w + o.xy * h]
            .into_iter()
            .min()
            .unwrap()
            * 65536;
        let ty = -[0, o.yx * w, o.yy * h, o.yx * w + o.yy * h]
            .into_iter()
            .min()
            .unwrap()
            * 65536;
        vec![
            o.xx * 65536,
            o.yx * 65536,
            0,
            o.xy * 65536,
            o.yy * 65536,
            0,
            tx,
            ty,
            1 << 30,
        ]
    }
    #[test]
    fn explicit_bounds_preserve_the_oriented_display_ratio_after_encoder_alignment() {
        let facts = video();
        let mut intent = config();
        intent.video_adjustments = Some(VideoAdjustments {
            rotation: VideoRotation::Clockwise90,
            ..Default::default()
        });
        intent.video_size = super::super::VideoSize::Custom;
        intent.custom_width = Some(64);
        intent.custom_height = Some(128);
        let (w, h) =
            super::super::configured_encoding_dimensions(&facts, &intent, "yuv420p").unwrap();
        assert_eq!((w, h), (76, 128));
        let plan = transform_plan(&facts, &intent, w, h).unwrap().unwrap();
        let sar = plan.output_sample_aspect_ratio;
        assert_eq!((sar.numerator, sar.denominator), (72, 95));
        assert_eq!(
            i128::from(w) * i128::from(sar.numerator) * 20,
            i128::from(h) * i128::from(sar.denominator) * 9
        );
    }

    #[test]
    fn camera_rebasing_reflections_and_user_transforms_compose_without_angle_guessing() {
        for q in 0..4 {
            for mirror in [false, true] {
                let source = if mirror {
                    Orientation::rotation(q).then(Orientation::H)
                } else {
                    Orientation::rotation(q)
                };
                let mut facts = video();
                facts.display_matrix = matrix(source, 160, 96);
                assert_eq!(source_orientation(&facts).unwrap(), source);
                for (turn, rotation) in [
                    (0, VideoRotation::Zero),
                    (1, VideoRotation::Clockwise90),
                    (2, VideoRotation::Clockwise180),
                    (3, VideoRotation::Clockwise270),
                ] {
                    for h in [false, true] {
                        for v in [false, true] {
                            let mut intent = config();
                            intent.video_adjustments = Some(VideoAdjustments {
                                rotation,
                                horizontal_flip: h,
                                vertical_flip: v,
                            });
                            if !intent.video_adjustments.as_ref().unwrap().active() {
                                continue;
                            }
                            let mut expected = source.then(Orientation::rotation(turn));
                            if h {
                                expected = expected.then(Orientation::H);
                            }
                            if v {
                                expected = expected.then(Orientation::V);
                            }
                            let oriented = oriented_video(&facts, &intent).unwrap();
                            assert_eq!(
                                oriented.width.zip(oriented.height),
                                Some(if expected.swaps() {
                                    (96, 160)
                                } else {
                                    (160, 96)
                                })
                            );
                            let sar = oriented.sample_aspect_ratio.as_ref().unwrap();
                            assert_eq!(
                                (sar.numerator, sar.denominator),
                                if expected.swaps() { (3, 4) } else { (4, 3) }
                            );
                            let plan = transform_plan(
                                &facts,
                                &intent,
                                oriented.width.unwrap(),
                                oriented.height.unwrap(),
                            )
                            .unwrap()
                            .unwrap();
                            let actual = if plan.flip_horizontal {
                                Orientation::rotation(plan.clockwise_quarter_turns)
                                    .then(Orientation::H)
                            } else {
                                Orientation::rotation(plan.clockwise_quarter_turns)
                            };
                            assert_eq!(actual, expected);
                        }
                    }
                }
            }
        }
    }
    #[test]
    fn unsupported_display_transforms_only_reject_active_baking_and_promotions_keep_ten_bits() {
        for matrix in [
            vec![46341, 46341, 0, -46341, 46341, 0, 0, 0, 1 << 30],
            vec![65536, 0, 0, 8192, 65536, 0, 0, 0, 1 << 30],
            vec![65536, 0, 1, 0, 65536, 0, 0, 0, 1 << 30],
            vec![65536, 0, 0, 0, 65536, 0, 12 * 65536, 0, 1 << 30],
        ] {
            let mut facts = video();
            facts.display_matrix = matrix;
            let mut intent = config();
            assert!(oriented_video(&facts, &intent).is_ok());
            intent.video_adjustments = Some(VideoAdjustments {
                rotation: VideoRotation::Clockwise90,
                ..Default::default()
            });
            assert!(oriented_video(&facts, &intent).is_err());
        }
        let mut facts = video();
        facts.height = Some(90);
        facts.pixel_format = Some("yuv422p10le".into());
        facts.bit_depth = Some(10);
        let mut intent = config();
        intent.video_adjustments = Some(VideoAdjustments {
            rotation: VideoRotation::Clockwise90,
            ..Default::default()
        });
        let plan = transform_plan(&facts, &intent, 90, 160).unwrap().unwrap();
        assert_eq!(
            plan.intermediate_pixel_format.as_deref(),
            Some("yuv444p10le")
        );
        assert!(plan
            .filters()
            .unwrap()
            .starts_with("format=pix_fmts=yuv444p10le,transpose=clock,"));
        let old:ConversionConfig=serde_json::from_value(json!({"operation":"VideoConverter","format":"Matroska","video_codec":"Automatic","audio_codec":"Automatic","video_size":"Original","frame_rate":"Original","quality":23,"file_name":"old"})).unwrap();
        assert!(old.video_adjustments.is_none());
        assert!(transform_plan(&facts, &old, 160, 90).unwrap().is_none());
    }
}
