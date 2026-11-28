//! One direct-library execution owner. Corrective work is bounded and never
//! replaces a valid first result until the alternative has passed validation.
use super::{compression, convert, verify_output, Plan, ProcessingProgress};
use crate::{
    media::{self, input::Input},
    CancellationToken, EngineError, MediaInfo, MediaProvider,
};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

fn storage(error: impl std::fmt::Display) -> EngineError {
    EngineError::Storage {
        detail: error.to_string(),
    }
}

fn canceled(token: &CancellationToken) -> Result<(), EngineError> {
    if token.is_cancelled() {
        Err(EngineError::Cancelled)
    } else {
        Ok(())
    }
}

struct CorrectiveDirectory(PathBuf);
impl CorrectiveDirectory {
    fn create(path: &Path) -> Result<Self, EngineError> {
        let parent = path
            .parent()
            .ok_or_else(|| storage("The working output has no private parent directory."))?;
        let directory = parent.join(format!("corrective-{}", uuid::Uuid::new_v4()));
        // An exclusive directory proves ownership even if the encoder fails
        // before creating its output. Never delete an occupied unknown path.
        fs::create_dir(&directory).map_err(storage)?;
        Ok(Self(directory))
    }
}
impl Drop for CorrectiveDirectory {
    fn drop(&mut self) {
        // All FFmpeg/input owners return before this guard is dropped, including
        // cancellation and error paths. The outer job retains its first result.
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[allow(clippy::too_many_arguments)]
fn run(
    provider: Arc<dyn MediaProvider>,
    locator: &str,
    source: &MediaInfo,
    actual_plan: &Plan,
    canonical: &Plan,
    path: &Path,
    token: Arc<CancellationToken>,
    progress: &mut impl FnMut(ProcessingProgress) -> Result<(), EngineError>,
) -> Result<MediaInfo, EngineError> {
    canceled(&token)?;
    let opened = provider.open_source(locator.into())?;
    let input = Input::open(opened, provider, token.clone())?;
    if input.source_identity()? != source.source_identity {
        return Err(EngineError::SourceAccess {
            detail:
                "The original source changed after inspection. Select it again before processing."
                    .into(),
        });
    }
    canceled(&token)?;
    // Native access and source verification are preparation, including when
    // reopening the source for a corrective attempt.
    progress(ProcessingProgress::Started)?;
    convert(
        input,
        source,
        actual_plan,
        path,
        token.clone(),
        &mut *progress,
    )?;
    canceled(&token)?;
    progress(ProcessingProgress::Validating)?;
    let info = media::inspect_file(path, token.clone())?;
    // A corrective bitrate must preserve every accepted media requirement.
    // Size attainment is a separate Jobs decision, never a validity shortcut.
    verify_output(&info, source, canonical)?;
    canceled(&token)?;
    Ok(info)
}

/// Execute at most two real encodes: the accepted plan and, only after measuring
/// a valid target miss, one applicable bitrate correction. No native route is
/// attempted here; native processing integration remains a separate work unit.
pub(crate) fn execute_conversion(
    provider: Arc<dyn MediaProvider>,
    locator: &str,
    source: &MediaInfo,
    plan: &Plan,
    path: &Path,
    token: Arc<CancellationToken>,
    mut progress: impl FnMut(ProcessingProgress) -> Result<(), EngineError>,
) -> Result<MediaInfo, EngineError> {
    // Coded duration includes lossy priming/padding, and WSOLA boundaries are
    // content-dependent. Analyze the accepted graph before an EOF fade or
    // normalization, checking actual decoded timing and measurable finite audio.
    // This never encodes intermediate samples.
    // This execution-local plan never rewrites the persisted accepted request.
    let measured_plan = if plan.streams.iter().any(|stream| {
        stream
            .audio_filters
            .as_ref()
            .is_some_and(|filters| filters.needs_measurement())
    }) {
        canceled(&token)?;
        let opened = provider.open_source(locator.into())?;
        let input = Input::open(opened, provider.clone(), token.clone())?;
        if input.source_identity()? != source.source_identity {
            return Err(EngineError::SourceAccess {
                detail:
                    "The original source changed before audio timing analysis. Select it again."
                        .into(),
            });
        }
        let counts = super::transcode::measure_audio_duration(
            input,
            source,
            plan,
            token.clone(),
            |position| progress(ProcessingProgress::AnalysisTimestamp(position)),
        )?;
        // The count pass owns and drops its first input before run() reopens.
        let mut measured_plan = plan.clone();
        for (index, samples, rate) in counts {
            measured_plan
                .streams
                .iter_mut()
                .find(|stream| stream.index == index)
                .and_then(|stream| stream.audio_filters.as_mut())
                .ok_or_else(|| EngineError::Internal {
                    detail: "Measured audio track is absent from the accepted plan.".into(),
                })?
                .apply_measurement(samples, rate)?;
        }
        if measured_plan.target_size_bytes.is_some() {
            compression::rebudget_measured_target(source, &mut measured_plan)?;
        }
        Some(measured_plan)
    } else {
        None
    };
    let plan = measured_plan.as_ref().unwrap_or(plan);
    let first = run(
        provider.clone(),
        locator,
        source,
        plan,
        plan,
        path,
        token.clone(),
        &mut progress,
    )?;
    let first_size = fs::symlink_metadata(path).map_err(storage)?;
    if !first_size.is_file() || first_size.len() == 0 {
        return Err(EngineError::InvalidMedia {
            detail: "The finalized working output is not a nonempty owned file.".into(),
        });
    }
    let Some(corrected) = compression::corrective_plan(source, plan, first_size.len())? else {
        return Ok(first);
    };
    canceled(&token)?;
    let scratch = CorrectiveDirectory::create(path)?;
    let alternative = scratch
        .0
        .join(format!("output.{}", plan.output()?.extension));
    progress(ProcessingProgress::Restarted)?;
    let candidate = run(
        provider,
        locator,
        source,
        &corrected,
        plan,
        &alternative,
        token.clone(),
        &mut progress,
    );
    let candidate = match candidate {
        Ok(candidate) => candidate,
        Err(
            error @ (EngineError::Cancelled
            | EngineError::SourceAccess { .. }
            | EngineError::Storage { .. }
            | EngineError::InvalidStorageRoot
            | EngineError::Recovery { .. }
            | EngineError::Platform { .. }
            | EngineError::Internal { .. }),
        ) => return Err(error),
        // An inapplicable/failed corrective encode cannot invalidate the valid
        // first result. It remains a truthful Target Not Met candidate, never a
        // successful target-size conversion. There is no additional retry loop.
        Err(_) => return Ok(first),
    };
    canceled(&token)?;
    let candidate_size = fs::symlink_metadata(&alternative).map_err(storage)?;
    if !candidate_size.is_file() || candidate_size.len() == 0 {
        return Err(EngineError::InvalidMedia {
            detail: "The corrective output is not a nonempty owned file.".into(),
        });
    }
    if candidate_size.len() >= first_size.len() {
        return Ok(first);
    }
    // convert() exclusively created the first path and committed both files.
    // Both paths belong to this worker's private job directory. Rename never
    // targets a user original or published destination.
    fs::rename(&alternative, path).map_err(storage)?;
    fs::File::open(path.parent().unwrap())
        .and_then(|directory| directory.sync_all())
        .map_err(storage)?;
    canceled(&token)?;
    Ok(candidate)
}
