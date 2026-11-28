use crate::{
    configuration::{audio_timing, invalid, AudioTiming},
    AudioAdjustments, AudioLoudnessPolicy, EngineError, MediaInfo, StreamInfo,
};
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
    #[serde(default)]
    pub(super) speed: Option<AudioSpeedPlan>,
    #[serde(default)]
    normalization: Option<AudioLoudnessPolicy>,
    // Missing in v1/v2: retain their frozen nominal fade graph on restart.
    #[serde(default)]
    measure_decoded_duration: bool,
    #[serde(skip)]
    measured: Option<(u64, u32)>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct AudioSpeedPlan {
    pub speed_milli: u32,
    pub preserve_pitch: bool,
    pub input_rate: u32,
    pub processing_rate: u32,
    pub playback_rate: u32,
    pub timing: AudioTiming,
}

fn speed_rates(
    input_rate: u32,
    speed: u32,
    preserve_pitch: bool,
) -> Result<(u32, u32), EngineError> {
    if !(1..=768000).contains(&input_rate) || !(500..=2000).contains(&speed) {
        return Err(invalid("The accepted audio speed sampling is invalid."));
    }
    let exact = u64::from(input_rate) * u64::from(speed) % 1000 == 0;
    let processing = if preserve_pitch || exact {
        input_rate
    } else {
        input_rate.div_ceil(48000) * 48000
    };
    let playback = if preserve_pitch {
        processing
    } else {
        u32::try_from(u64::from(processing) * u64::from(speed) / 1000)
            .map_err(|_| invalid("The speed processing sample rate exceeds supported bounds."))?
    };
    Ok((processing, playback))
}

pub(crate) fn audio_plan(
    source: &MediaInfo,
    stream: &StreamInfo,
    settings: Option<&AudioAdjustments>,
) -> Result<Option<AudioFilterPlan>, EngineError> {
    let Some(settings) = settings.filter(|settings| settings.active()) else {
        return Ok(None);
    };
    settings.validate()?;
    let normalization = if settings.normalize_loudness {
        Some(
            AudioLoudnessPolicy::available()
                .ok_or_else(|| invalid("Loudness normalization is unavailable in this build."))?,
        )
    } else {
        None
    };
    let timing = audio_timing(source, stream, Some(settings))?;
    let duration = timing.output_duration_us;
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
    // Every newly accepted EOF fade uses decoded sample timing. Coded duration
    // can include lossy encoder padding even at nominal playback speed.
    let speed = if settings.speed_milli != 1000
        || settings.fade_out_us > 0
        || settings.normalize_loudness
    {
        if settings.speed_milli != 1000
            && (!source.chapters.is_empty()
                || source.streams.iter().any(|stream| {
                    stream.kind != crate::StreamKind::Audio
                        && !stream
                            .video
                            .as_ref()
                            .is_some_and(|video| video.attached_picture)
                }))
        {
            return Err(invalid("Audio speed cannot silently retime chapters, video, subtitles or data tracks. This source needs an explicit compatible timeline operation."));
        }
        let input_rate = stream
            .audio
            .as_ref()
            .and_then(|audio| audio.sample_rate)
            .filter(|rate| (1..=768000).contains(rate))
            .ok_or_else(|| {
                invalid("Audio speed requires a known supported source sampling rate.")
            })?;
        if duration.is_none() {
            return Err(invalid(
                "Audio speed requires a known positive track duration for output verification.",
            ));
        }
        let (processing_rate, playback_rate) =
            speed_rates(input_rate, settings.speed_milli, settings.preserve_pitch)?;
        let mut needed = vec![c"asetpts", c"asettb", c"aresample", c"aformat"];
        if settings.speed_milli != 1000 {
            needed.push(if settings.preserve_pitch {
                c"atempo"
            } else {
                c"asetrate"
            });
        }
        for name in needed {
            if unsafe { crate::ffmpeg::sys::avfilter_get_by_name(name.as_ptr()).is_null() } {
                return Err(invalid(
                    "A required audio speed filter is unavailable in this build.",
                ));
            }
        }
        Some(AudioSpeedPlan {
            speed_milli: settings.speed_milli,
            preserve_pitch: settings.preserve_pitch,
            input_rate,
            processing_rate,
            playback_rate,
            timing: timing.clone(),
        })
    } else {
        None
    };
    Ok(Some(AudioFilterPlan {
        gain_centidb: settings.gain_centidb,
        fade_in_us: settings.fade_in_us,
        fade_out_us: settings.fade_out_us,
        duration_us: duration.unwrap_or(0),
        start_us: timing.output_start_us,
        speed,
        normalization,
        measure_decoded_duration: settings.normalize_loudness
            || settings.fade_out_us > 0
            || settings.speed_milli != 1000
                && !stream.codec.starts_with("pcm_")
                && !matches!(stream.codec.as_str(), "flac" | "alac" | "wavpack"),
        measured: None,
    }))
}

impl AudioFilterPlan {
    pub(super) fn normalization_enabled(&self) -> bool {
        self.normalization.is_some()
    }

    pub(crate) fn timing(&self) -> Option<AudioTiming> {
        self.speed.as_ref().map(|speed| {
            let mut timing = speed.timing.clone();
            if let Some((samples, rate)) = self.measured {
                timing.output_duration_us =
                    Some((u128::from(samples) * 1000000 / u128::from(rate)) as u64);
            }
            timing
        })
    }

    pub(crate) fn timing_tolerance_us(
        &self,
        source: &StreamInfo,
    ) -> Result<Option<u64>, EngineError> {
        let Some(speed) = &self.speed else {
            return Ok(None);
        };
        if self.measured.is_some() {
            return Ok(Some(0));
        }
        if !(500..=2000).contains(&speed.speed_milli) || speed.input_rate == 0 {
            return Err(invalid("The accepted audio timing scale is invalid."));
        }
        // The source snapshot is part of the immutable accepted request. A
        // coarse source PTS quantum survives sample-clock gap reconstruction;
        // compare header-derived duration at that precision, scaled by speed.
        let source_tick = source
            .time_base
            .as_ref()
            .filter(|base| base.numerator > 0 && base.denominator > 0)
            .map(|base| {
                let numerator = base.numerator as u128 * 1_000_000 * 1000;
                let denominator = base.denominator as u128 * u128::from(speed.speed_milli);
                u64::try_from(numerator.div_ceil(denominator)).map_err(|_| {
                    invalid("The source audio timestamp precision exceeds supported bounds.")
                })
            })
            .transpose()?
            .unwrap_or(0);
        let window = if speed.preserve_pitch && speed.speed_milli != 1000 {
            // Pinned atempo uses the next power of two of rate/24 for its
            // WSOLA window. Two windows bound content-dependent boundaries.
            let window = (speed.input_rate / 24).max(1).next_power_of_two();
            (u64::from(window) * 2 * 1_000_000).div_ceil(u64::from(speed.input_rate))
        } else {
            0
        };
        Ok(Some(source_tick.checked_add(window).ok_or_else(|| {
            invalid("The audio timing tolerance exceeds supported bounds.")
        })?))
    }

    pub(crate) fn needs_measurement(&self) -> bool {
        (self.measure_decoded_duration || self.normalization.is_some()) && self.measured.is_none()
    }

    pub(super) fn measurement_clone(&self) -> Self {
        let mut measured = self.clone();
        measured.gain_centidb = 0;
        measured.fade_in_us = 0;
        measured.fade_out_us = 0;
        measured
    }

    pub(crate) fn apply_measurement(&mut self, samples: u64, rate: u32) -> Result<(), EngineError> {
        if samples == 0 || rate == 0 {
            return Err(invalid("The audio timing measurement contains no samples."));
        }
        let duration = u64::try_from(u128::from(samples) * 1000000 / u128::from(rate))
            .map_err(|_| invalid("The measured audio duration exceeds supported bounds."))?;
        if self
            .fade_in_us
            .checked_add(self.fade_out_us)
            .is_none_or(|fades| fades > duration)
        {
            return Err(invalid("The requested fades exceed the actual decoded output duration. Reduce their length."));
        }
        self.measured = Some((samples, rate));
        Ok(())
    }

    pub(super) fn speed_filters(
        &self,
        output_rate: i32,
        output_layout: &str,
        output_sample: &str,
        measure_only: bool,
    ) -> Result<Option<String>, EngineError> {
        let Some(speed) = &self.speed else {
            return Ok(None);
        };
        if speed_rates(speed.input_rate, speed.speed_milli, speed.preserve_pitch)?
            != (speed.processing_rate, speed.playback_rate)
            || speed.timing.output_start_us != self.start_us
            || speed.timing.output_duration_us != Some(self.duration_us)
        {
            return Err(invalid(
                "The accepted audio speed sampling or timing is invalid.",
            ));
        }
        let mut chain = format!("asettb=1/{},asetpts=N,", speed.input_rate);
        if speed.processing_rate != speed.input_rate {
            chain.push_str(&format!("aresample={},", speed.processing_rate));
        }
        if speed.speed_milli != 1000 {
            if speed.preserve_pitch {
                chain.push_str(&format!(
                    "atempo={:.3},",
                    f64::from(speed.speed_milli) / 1000.0
                ));
            } else {
                chain.push_str(&format!("asetrate={},", speed.playback_rate));
            }
        }
        if output_rate <= 0 {
            return Err(invalid("The accepted output sampling rate is invalid."));
        }
        let numerator = i128::from(self.start_us) * i128::from(output_rate);
        let rounded = if numerator >= 0 {
            (numerator + 500_000) / 1_000_000
        } else {
            -((-numerator + 500_000) / 1_000_000)
        };
        let offset = i64::try_from(rounded)
            .map_err(|_| invalid("The output audio sample origin exceeds supported bounds."))?;
        // Integer sample ticks avoid N/SR/TB floating truncation shifting a
        // frame by one sample, which is audible at a short fade boundary.
        chain.push_str(&format!("aresample={output_rate},aformat=sample_fmts=dbl:sample_rates={output_rate}:channel_layouts={output_layout},"));
        if let Some(policy) = &self.normalization {
            policy.validate()?;
            if measure_only {
                // Read only integrated LUFS. True-peak metering is deliberately
                // left to loudnorm; no private ebur128 resampler tail is used.
                chain.push_str("ebur128@velmux_normalization_input=peak=none:framelog=quiet,");
            }
            // Dynamic mode measures actual EBU R128 loudness within the filter.
            // It requires 192 kHz internally; explicitly restore the accepted
            // sampling and integer clock before deliberate gain and fades.
            chain.push_str(&format!("loudnorm=linear=false:I={:.2}:TP={:.2}:LRA={:.2},aresample={output_rate},aformat=sample_fmts=dbl:sample_rates={output_rate}:channel_layouts={output_layout},",
                f64::from(policy.target_centi_lufs) / 100.0,
                f64::from(policy.maximum_true_peak_centidb) / 100.0,
                f64::from(policy.range_centi_lu) / 100.0));
        }
        chain.push_str(&format!("asettb=1/{output_rate},asetpts=N{offset:+},"));
        chain.push_str(&self.filters()?);
        chain.push_str(&format!("aformat=sample_fmts={output_sample}:sample_rates={output_rate}:channel_layouts={output_layout}"));
        Ok(Some(chain))
    }

    pub(super) fn filters(&self) -> Result<String, EngineError> {
        if self.normalization.is_some() && self.speed.is_none() {
            return Err(invalid(
                "The accepted normalization plan has no sample-clock timing.",
            ));
        }
        let settings = AudioAdjustments {
            gain_centidb: self.gain_centidb,
            fade_in_us: self.fade_in_us,
            fade_out_us: self.fade_out_us,
            speed_milli: self.speed.as_ref().map_or(1000, |speed| speed.speed_milli),
            preserve_pitch: self.speed.as_ref().is_none_or(|speed| speed.preserve_pitch),
            normalize_loudness: self.normalization.is_some(),
        };
        settings.validate()?;
        let duration_us = self
            .timing()
            .and_then(|timing| timing.output_duration_us)
            .unwrap_or(self.duration_us);
        if (self.fade_in_us != 0 || self.fade_out_us != 0)
            && self.fade_in_us.saturating_add(self.fade_out_us) > duration_us
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
                (self.start_us as f64 + (duration_us - self.fade_out_us) as f64) / 1_000_000.0,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalization_policy_is_frozen_and_measurement_preserves_the_actual_stage() {
        let mut accepted: AudioFilterPlan = serde_json::from_str(r#"{"gain_centidb":-300,"fade_in_us":100000,"fade_out_us":250000,"duration_us":2000000,"start_us":0}"#).unwrap();
        assert!(accepted.normalization.is_none());
        accepted.speed = Some(AudioSpeedPlan {
            speed_milli: 1000,
            preserve_pitch: true,
            input_rate: 48000,
            processing_rate: 48000,
            playback_rate: 48000,
            timing: AudioTiming {
                input_start_us: 0,
                output_start_us: 0,
                input_duration_us: Some(2000000),
                output_duration_us: Some(2000000),
            },
        });
        accepted.normalization = Some(AudioLoudnessPolicy::default());
        assert!(
            accepted.needs_measurement(),
            "Normalization must not bypass analysis even in an older flag shape"
        );
        let measurement = accepted.measurement_clone();
        assert_eq!(measurement.normalization, accepted.normalization);
        let measured_chain = measurement
            .speed_filters(48000, "stereo", "dbl", true)
            .unwrap()
            .unwrap();
        assert!(measured_chain.contains("ebur128@velmux_normalization_input"));
        assert!(measured_chain.contains("loudnorm=linear=false:I=-16.00:TP=-1.00:LRA=11.00"));
        assert!(!measured_chain.contains("volume="));
        assert!(!measured_chain.contains("afade="));
        accepted.apply_measurement(96000, 48000).unwrap();
        let chain = accepted
            .speed_filters(48000, "stereo", "s16", false)
            .unwrap()
            .unwrap();
        assert!(chain.find("channel_layouts=stereo").unwrap() < chain.find("loudnorm=").unwrap());
        assert!(chain.find("loudnorm=").unwrap() < chain.find("volume=").unwrap());
        assert!(chain.find("volume=").unwrap() < chain.find("afade=").unwrap());
        let saved = serde_json::to_string(&accepted).unwrap();
        let restored: AudioFilterPlan = serde_json::from_str(&saved).unwrap();
        assert_eq!(restored.normalization, accepted.normalization);
        assert!(
            restored.needs_measurement(),
            "Execution measurements are not accepted intent"
        );
        accepted.normalization.as_mut().unwrap().target_centi_lufs = i32::MIN;
        assert!(accepted
            .speed_filters(48000, "stereo", "s16", false)
            .is_err());
    }

    #[test]
    fn legacy_fade_policy_stays_frozen_and_new_policy_uses_measured_sample_endpoint() {
        let saved: AudioFilterPlan = serde_json::from_str(r#"{"gain_centidb":0,"fade_in_us":0,"fade_out_us":250000,"duration_us":2160000,"start_us":0}"#).unwrap();
        assert!(!saved.needs_measurement());
        assert!(saved.timing().is_none());
        assert!(saved.filters().unwrap().contains("st=1.910000"));
        let mut current = saved.clone();
        current.measure_decoded_duration = true;
        current.speed = Some(AudioSpeedPlan {
            speed_milli: 1000,
            preserve_pitch: true,
            input_rate: 8000,
            processing_rate: 8000,
            playback_rate: 8000,
            timing: AudioTiming {
                input_start_us: 0,
                output_start_us: 0,
                input_duration_us: Some(2160000),
                output_duration_us: Some(2160000),
            },
        });
        assert!(current.needs_measurement());
        current.apply_measurement(16000, 8000).unwrap();
        assert_eq!(current.timing().unwrap().output_duration_us, Some(2000000));
        let chain = current
            .speed_filters(8000, "mono", "s16", false)
            .unwrap()
            .unwrap();
        assert!(chain.contains("st=1.750000"));
        assert!(chain.contains("asettb=1/8000,asetpts=N"));
        assert!(!chain.contains("atempo="));
        assert!(!chain.contains("asetrate="));
        assert!(!current.needs_measurement());
        let persisted = serde_json::to_value(&current).unwrap();
        assert!(persisted.get("measured").is_none());
    }
}
