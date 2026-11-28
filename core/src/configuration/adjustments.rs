use super::invalid;
use crate::{EngineError, MediaInfo};
use serde::{Deserialize, Serialize};

/// Sample-domain adjustments for Audio Converter. Integer hundredths of a
/// decibel avoid non-finite inputs and preserve exact workspace intent.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, uniffi::Record)]
#[serde(default)]
pub struct AudioAdjustments {
    pub gain_centidb: i32,
    pub fade_in_us: u64,
    pub fade_out_us: u64,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct AudioAdjustmentLimits {
    pub minimum_gain_centidb: i32,
    pub maximum_gain_centidb: i32,
    pub maximum_fade_us: Option<u64>,
}

pub(crate) fn duration(source: &MediaInfo) -> Option<u64> {
    source
        .streams
        .iter()
        .find_map(|stream| stream.audio.as_ref().and(stream.duration_us))
        .or(source.duration_us)
        .filter(|duration| *duration > 0)
        .map(|duration| duration as u64)
}

pub(crate) fn limits(source: &MediaInfo) -> AudioAdjustmentLimits {
    AudioAdjustmentLimits {
        minimum_gain_centidb: -6000,
        maximum_gain_centidb: 2400,
        maximum_fade_us: duration(source).map(|duration| duration.min(600_000_000)),
    }
}

impl AudioAdjustments {
    pub(crate) fn validate(&self) -> Result<(), EngineError> {
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
        self.gain_centidb != 0 || self.fade_in_us != 0 || self.fade_out_us != 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
