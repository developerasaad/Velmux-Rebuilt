use crate::{configuration::invalid, AudioAdjustments, EngineError, MediaInfo};
use serde::{Deserialize, Serialize};

/// An accepted sample filter plan is immutable and does not re-read evolving
/// workspace defaults. Missing plans preserve earlier accepted processing.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct AudioFilterPlan {
    gain_centidb: i32,
    fade_in_us: u64,
    fade_out_us: u64,
    duration_us: u64,
    start_us: i64,
}

pub(crate) fn audio_plan(
    source: &MediaInfo,
    settings: Option<&AudioAdjustments>,
) -> Result<Option<AudioFilterPlan>, EngineError> {
    let Some(settings) = settings.filter(|settings| settings.active()) else {
        return Ok(None);
    };
    settings.validate()?;
    let duration = crate::configuration::audio_duration(source);
    if (settings.fade_in_us != 0 || settings.fade_out_us != 0)
        && duration.is_none_or(|duration| {
            settings.fade_in_us.saturating_add(settings.fade_out_us) > duration
        })
    {
        return Err(invalid("Audio fades require a known duration, and their combined length must not exceed the audio duration."));
    }
    let filters = [
        (settings.gain_centidb != 0, c"volume"),
        (
            settings.fade_in_us != 0 || settings.fade_out_us != 0,
            c"afade",
        ),
        (settings.gain_centidb > 0, c"alimiter"),
    ];
    for (needed, name) in filters {
        if needed && unsafe { crate::ffmpeg::sys::avfilter_get_by_name(name.as_ptr()).is_null() } {
            return Err(invalid(
                "A required audio adjustment filter is unavailable in this build.",
            ));
        }
    }
    Ok(Some(AudioFilterPlan {
        gain_centidb: settings.gain_centidb,
        fade_in_us: settings.fade_in_us,
        fade_out_us: settings.fade_out_us,
        duration_us: duration.unwrap_or(0),
        start_us: source
            .streams
            .iter()
            .find(|stream| stream.audio.is_some())
            .and_then(|stream| stream.start_time_us)
            .unwrap_or_else(|| super::common_origin(source).unwrap_or(0))
            .checked_sub(super::common_origin(source).unwrap_or(0))
            .ok_or_else(|| invalid("The audio timeline exceeds the supported range."))?,
    }))
}

impl AudioFilterPlan {
    pub(super) fn filters(&self) -> Result<String, EngineError> {
        let settings = AudioAdjustments {
            gain_centidb: self.gain_centidb,
            fade_in_us: self.fade_in_us,
            fade_out_us: self.fade_out_us,
        };
        settings.validate()?;
        if (self.fade_in_us != 0 || self.fade_out_us != 0)
            && self.fade_in_us.saturating_add(self.fade_out_us) > self.duration_us
        {
            return Err(invalid("The accepted fade interval is invalid."));
        }
        let mut filters = String::new();
        if self.gain_centidb != 0 {
            filters.push_str(&format!(
                "volume={:.2}dB:precision=double,",
                f64::from(self.gain_centidb) / 100.0
            ));
        }
        if self.fade_in_us != 0 {
            filters.push_str(&format!(
                "afade=t=in:st={:.6}:d={:.6}:curve=tri,",
                self.start_us as f64 / 1_000_000.0,
                self.fade_in_us as f64 / 1_000_000.0
            ));
        }
        if self.fade_out_us != 0 {
            filters.push_str(&format!(
                "afade=t=out:st={:.6}:d={:.6}:curve=tri,",
                (self.start_us as f64 + (self.duration_us - self.fade_out_us) as f64) / 1_000_000.0,
                self.fade_out_us as f64 / 1_000_000.0
            ));
        }
        if self.gain_centidb > 0 {
            // Preserve channels and sample timing, compensate lookahead/tail,
            // disable automatic makeup gain and leave 0.45 dB peak headroom.
            filters.push_str("alimiter=limit=0.95:attack=5:release=50:level=disabled:latency=1,");
        }
        Ok(filters)
    }
}
