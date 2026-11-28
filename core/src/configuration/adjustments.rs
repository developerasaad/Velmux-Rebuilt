use super::invalid;
use crate::{EngineError, MediaInfo, StreamInfo, StreamKind};
use serde::{Deserialize, Serialize};

/// Sample-domain adjustments for Audio Converter. Integer hundredths of a
/// decibel avoid non-finite inputs and preserve exact workspace intent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, uniffi::Record)]
#[serde(default)]
pub struct AudioAdjustments {
    pub gain_centidb: i32,
    pub fade_in_us: u64,
    pub fade_out_us: u64,
    pub speed_milli: u32,
    pub preserve_pitch: bool,
}

impl Default for AudioAdjustments {
    fn default() -> Self {
        Self {
            gain_centidb: 0,
            fade_in_us: 0,
            fade_out_us: 0,
            speed_milli: 1000,
            preserve_pitch: true,
        }
    }
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct AudioAdjustmentLimits {
    pub minimum_gain_centidb: i32,
    pub maximum_gain_centidb: i32,
    pub maximum_fade_us: Option<u64>,
    pub minimum_speed_milli: u32,
    pub maximum_speed_milli: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct AudioTiming {
    pub input_start_us: i64,
    pub output_start_us: i64,
    pub input_duration_us: Option<u64>,
    pub output_duration_us: Option<u64>,
}

pub(crate) fn stream_duration(source: &MediaInfo, stream: &StreamInfo) -> Option<u64> {
    if let Some(duration) = stream.duration_us.filter(|value| *value > 0) {
        return Some(duration as u64);
    }
    let timed = source
        .streams
        .iter()
        .filter(|stream| stream.kind == StreamKind::Audio || super::primary_video(stream))
        .count();
    (timed == 1
        && stream
            .start_time_us
            .zip(source.start_time_us)
            .is_some_and(|(left, right)| left == right))
    .then(|| {
        source
            .duration_us
            .filter(|value| *value > 0)
            .map(|value| value as u64)
    })
    .flatten()
}

pub(crate) fn scaled_time(value: i64, speed_milli: u32) -> Result<i64, EngineError> {
    if !(500..=2000).contains(&speed_milli) {
        return Err(invalid(
            "Audio playback speed must be between 0.5 and 2 times.",
        ));
    }
    let numerator = i128::from(value) * 1000;
    let divisor = i128::from(speed_milli);
    let rounded = if numerator >= 0 {
        (numerator + divisor / 2) / divisor
    } else {
        -((-numerator + divisor / 2) / divisor)
    };
    i64::try_from(rounded)
        .map_err(|_| invalid("The transformed audio timeline exceeds supported bounds."))
}

pub(crate) fn timing(
    source: &MediaInfo,
    stream: &StreamInfo,
    settings: Option<&AudioAdjustments>,
) -> Result<AudioTiming, EngineError> {
    let speed = settings.map_or(1000, |settings| settings.speed_milli);
    let origin = crate::processing::common_origin(source).unwrap_or(0);
    let input_start_us = stream
        .start_time_us
        .unwrap_or(origin)
        .checked_sub(origin)
        .ok_or_else(|| invalid("The audio timeline exceeds supported bounds."))?;
    let input_duration_us = stream_duration(source, stream);
    let output_duration_us = input_duration_us
        .map(|duration| {
            let duration = i64::try_from(duration)
                .map_err(|_| invalid("The audio duration exceeds supported bounds."))?;
            u64::try_from(scaled_time(duration, speed)?)
                .map_err(|_| invalid("The transformed audio duration is invalid."))
        })
        .transpose()?;
    Ok(AudioTiming {
        input_start_us,
        output_start_us: scaled_time(input_start_us, speed)?,
        input_duration_us,
        output_duration_us,
    })
}

pub(crate) fn limits(
    source: &MediaInfo,
    settings: Option<&AudioAdjustments>,
) -> AudioAdjustmentLimits {
    AudioAdjustmentLimits {
        minimum_gain_centidb: -6000,
        maximum_gain_centidb: 2400,
        maximum_fade_us: source
            .streams
            .iter()
            .find(|stream| stream.kind == StreamKind::Audio)
            .and_then(|stream| timing(source, stream, settings).ok())
            .and_then(|timing| timing.output_duration_us)
            .map(|duration| duration.min(600_000_000)),
        minimum_speed_milli: 500,
        maximum_speed_milli: 2000,
    }
}

impl AudioAdjustments {
    pub(crate) fn validate(&self) -> Result<(), EngineError> {
        if !(500..=2000).contains(&self.speed_milli) {
            return Err(invalid(
                "Audio playback speed must be between 0.5 and 2 times.",
            ));
        }
        if !(-6_000..=2_400).contains(&self.gain_centidb) {
            return Err(invalid("Audio gain must be between -60 and +24 dB."));
        }
        if self.fade_in_us > 600_000_000 || self.fade_out_us > 600_000_000 {
            return Err(invalid(
                "Each audio fade must be between zero and 600 seconds.",
            ));
        }
        Ok(())
    }

    pub(crate) fn active(&self) -> bool {
        self.gain_centidb != 0
            || self.fade_in_us != 0
            || self.fade_out_us != 0
            || self.speed_milli != 1000
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_adjustments_default_speed_and_pitch_without_rewriting_intent() {
        let saved: AudioAdjustments = serde_json::from_str(
            r#"{"gain_centidb":-300,"fade_in_us":250000,"fade_out_us":500000}"#,
        )
        .unwrap();
        assert_eq!(saved.speed_milli, 1000);
        assert!(saved.preserve_pitch);
        assert_eq!(saved.gain_centidb, -300);
        for (value, speed, required) in [
            (2_000_000, 500, 4_000_000),
            (500_000, 2000, 250_000),
            (-500_000, 2000, -250_000),
            (2_000_000, 1333, 1_500_375),
        ] {
            assert_eq!(scaled_time(value, speed).unwrap(), required);
        }
        assert!(scaled_time(i64::MAX, 500).is_err());
        for speed in [0, 499, 2001, u32::MAX] {
            assert!(scaled_time(1, speed).is_err());
        }
    }

    #[test]
    fn gain_and_fade_inputs_are_bounded_and_defaults_are_inactive() {
        assert!(!AudioAdjustments::default().active());
        for gain in [-6000, 0, 2400] {
            assert!(AudioAdjustments {
                gain_centidb: gain,
                ..Default::default()
            }
            .validate()
            .is_ok());
        }
        for gain in [i32::MIN, -6001, 2401, i32::MAX] {
            assert!(AudioAdjustments {
                gain_centidb: gain,
                ..Default::default()
            }
            .validate()
            .is_err());
        }
        assert!(AudioAdjustments {
            fade_out_us: u64::MAX,
            ..Default::default()
        }
        .validate()
        .is_err());
    }
}
