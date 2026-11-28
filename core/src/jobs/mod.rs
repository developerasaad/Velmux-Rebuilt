mod preview;
mod store;
pub use preview::PreviewResult;

use crate::{
    configuration::{self, CompressionMode, ConversionConfig, Operation, TargetSize},
    media::{self, input::Input},
    processing::{self, Plan},
    CancellationToken, EngineError, MediaInfo, MediaProvider, OutputReservation, PublishedOutput,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::Read;
use std::{
    collections::HashMap,
    fs,
    path::PathBuf,
    sync::{Arc, Condvar, Mutex},
    thread::{self, JoinHandle},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use store::Store;

#[derive(Clone, Debug, Serialize, Deserialize, uniffi::Record)]
pub struct InspectedSource {
    pub id: String,
    pub locator: String,
    pub display_name: String,
    pub info: MediaInfo,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, uniffi::Enum)]
pub enum JobState {
    Queued,
    Preparing,
    Processing,
    Finalizing,
    Validating,
    Publishing,
    Canceling,
    Completed,
    Failed,
    Canceled,
    Interrupted,
    Paused,
    TargetNotMet,
}
impl JobState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Preparing => "preparing",
            Self::Processing => "processing",
            Self::Finalizing => "finalizing",
            Self::Validating => "validating",
            Self::Publishing => "publishing",
            Self::Canceling => "canceling",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Canceled => "canceled",
            Self::Interrupted => "interrupted",
            Self::Paused => "paused",
            Self::TargetNotMet => "target_not_met",
        }
    }
    pub(crate) fn terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Canceled)
    }
    fn active(self) -> bool {
        matches!(
            self,
            Self::Preparing
                | Self::Processing
                | Self::Finalizing
                | Self::Validating
                | Self::Publishing
                | Self::Canceling
        )
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, uniffi::Record)]
pub struct JobRecord {
    pub id: String,
    pub source_name: String,
    pub file_name: String,
    pub state: JobState,
    pub processed_us: i64,
    pub duration_us: Option<i64>,
    pub error: Option<String>,
    pub error_category: Option<String>,
    pub created_ms: i64,
    pub finished_ms: Option<i64>,
    pub source_size: Option<u64>,
    pub verified: bool,
    pub verified_size: Option<u64>,
    pub verified_hash: Option<String>,
    pub reservation: Option<OutputReservation>,
    pub output: Option<PublishedOutput>,
    /// Explicit consent to publish this retained verified output despite its
    /// missed target. The accepted request itself remains immutable.
    #[serde(default)]
    pub keep_target_miss: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct AcceptedRequest {
    #[serde(default = "legacy_accepted_request_version")]
    version: u32,
    submission_key: String,
    source: InspectedSource,
    plan: Plan,
}

const CURRENT_ACCEPTED_REQUEST_VERSION: u32 = 2;

fn legacy_accepted_request_version() -> u32 {
    1
}

#[uniffi::export(callback_interface)]
pub trait JobObserver: Send + Sync {
    fn on_jobs_changed(&self);
}

struct Control {
    stop: bool,
    wake: bool,
    active: Option<(String, Arc<CancellationToken>)>,
}

struct AccessEntry {
    // Native calls serialize only against another acquisition/release of this
    // locator; the registry lock never crosses an OS callback.
    acquired: Mutex<bool>,
}
struct AccessCount {
    entry: Arc<AccessEntry>,
    references: usize,
    preserve: bool,
}
struct SourceAccess {
    provider: Arc<dyn MediaProvider>,
    entries: Mutex<HashMap<String, AccessCount>>,
}
struct SourceAccessLease {
    owner: Arc<SourceAccess>,
    locator: String,
    entry: Arc<AccessEntry>,
    durable: bool,
    retired: std::sync::atomic::AtomicBool,
}
impl SourceAccess {
    fn claim(
        self: &Arc<Self>,
        locator: &str,
        durable: bool,
    ) -> Result<(Arc<SourceAccessLease>, Result<(), EngineError>), EngineError> {
        let entry = {
            let mut entries = self.entries.lock().map_err(internal)?;
            let count = entries
                .entry(locator.into())
                .or_insert_with(|| AccessCount {
                    entry: Arc::new(AccessEntry {
                        acquired: Mutex::new(false),
                    }),
                    references: 0,
                    preserve: false,
                });
            count.references += 1;
            count.entry.clone()
        };
        let lease = Arc::new(SourceAccessLease {
            owner: self.clone(),
            locator: locator.into(),
            entry: entry.clone(),
            durable,
            retired: std::sync::atomic::AtomicBool::new(false),
        });
        let result = {
            let mut acquired = entry
                .acquired
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if *acquired {
                Ok(())
            } else {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    self.provider.retain_source_access(locator.into())
                }))
                .unwrap_or_else(|_| {
                    Err(internal(
                        "The source provider could not retain document access.",
                    ))
                });
                if result.is_ok() {
                    *acquired = true;
                }
                result
            }
        };
        Ok((lease, result))
    }
    fn retain(self: &Arc<Self>, locator: &str) -> Result<Arc<SourceAccessLease>, EngineError> {
        let (lease, result) = self.claim(locator, false)?;
        result?;
        Ok(lease)
    }
}
impl Drop for SourceAccessLease {
    fn drop(&mut self) {
        let mut acquired = self
            .entry
            .acquired
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let last = {
            let mut entries = self
                .owner
                .entries
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let Some(count) = entries.get_mut(&self.locator) else {
                return;
            };
            count.references -= 1;
            // Closing a runtime cannot revoke an unfinished durable request's
            // access. Its next owner reconstructs references from SQLite.
            count.preserve |=
                self.durable && !self.retired.load(std::sync::atomic::Ordering::SeqCst);
            if count.references != 0 {
                return;
            }
            !count.preserve
        };
        if last {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                self.owner
                    .provider
                    .release_source_access(self.locator.clone());
            }));
            *acquired = false;
        }
        let mut entries = self
            .owner
            .entries
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        // A new claimant can register while native release runs, then waits on
        // this entry and re-acquires access before opening its descriptor.
        if entries
            .get(&self.locator)
            .is_some_and(|count| count.references == 0)
        {
            entries.remove(&self.locator);
        }
    }
}
#[derive(Clone)]
struct SourceAsset {
    source: InspectedSource,
    _access: Arc<SourceAccessLease>,
}
impl std::ops::Deref for SourceAsset {
    type Target = InspectedSource;
    fn deref(&self) -> &Self::Target {
        &self.source
    }
}
pub(crate) struct Runtime {
    root: PathBuf,
    pub provider: Arc<dyn MediaProvider>,
    store: Store,
    source_access: Arc<SourceAccess>,
    job_access: Mutex<HashMap<String, Arc<SourceAccessLease>>>,
    assets: Mutex<HashMap<String, SourceAsset>>,
    control: Mutex<Control>,
    changed: Condvar,
    observer: Mutex<Option<Arc<dyn JobObserver>>>,
    worker: Mutex<Option<JoinHandle<()>>>,
    submission: Mutex<()>,
    health: Mutex<Option<String>>,
    previews: Mutex<HashMap<String, preview::PreviewWork>>,
    preview_slot: Mutex<()>,
}
pub(crate) fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}
fn internal(error: impl std::fmt::Display) -> EngineError {
    EngineError::Internal {
        detail: error.to_string(),
    }
}
fn storage(error: impl std::fmt::Display) -> EngineError {
    EngineError::Storage {
        detail: error.to_string(),
    }
}
fn category(error: &EngineError) -> &'static str {
    match error {
        EngineError::Cancelled => "canceled",
        EngineError::SourceAccess { .. } => "source_access",
        EngineError::UnsupportedMedia { .. } => "unsupported_media",
        EngineError::InvalidMedia { .. } => "invalid_media",
        EngineError::Configuration { .. } => "configuration",
        EngineError::Decoding { .. } => "decoding",
        EngineError::Encoding { .. } => "encoding",
        EngineError::Filtering { .. } => "filtering",
        EngineError::Muxing { .. } => "muxing",
        EngineError::Storage { .. } | EngineError::InvalidStorageRoot => "storage",
        EngineError::Recovery { .. } => "recovery",
        EngineError::Platform { .. } => "platform",
        EngineError::Internal { .. } => "internal",
    }
}

impl Runtime {
    pub fn open(root: String, provider: Box<dyn MediaProvider>) -> Result<Arc<Self>, EngineError> {
        let root = PathBuf::from(root);
        if !root.is_absolute() {
            return Err(EngineError::InvalidStorageRoot);
        }
        fs::create_dir_all(&root).map_err(storage)?;
        let root = root.canonicalize().map_err(storage)?;
        let provider: Arc<dyn MediaProvider> = Arc::from(provider);
        let runtime = Arc::new(Self {
            store: Store::open(&root)?,
            root,
            source_access: Arc::new(SourceAccess {
                provider: provider.clone(),
                entries: Mutex::new(HashMap::new()),
            }),
            provider,
            job_access: Mutex::new(HashMap::new()),
            assets: Mutex::new(HashMap::new()),
            control: Mutex::new(Control {
                stop: false,
                wake: true,
                active: None,
            }),
            changed: Condvar::new(),
            observer: Mutex::new(None),
            worker: Mutex::new(None),
            submission: Mutex::new(()),
            health: Mutex::new(None),
            previews: Mutex::new(HashMap::new()),
            preview_slot: Mutex::new(()),
        });
        runtime.restore_source_access()?;
        runtime.restore()?;
        // Cache cleanup is best effort at startup. Explicit Clear Cache reports
        // storage errors; inability to remove disposable files must not disable
        // unrelated direct-source jobs or their restoration.
        let _ = runtime.clear_cache();
        let owned = runtime.clone();
        let worker = thread::Builder::new()
            .name("velmux-media-worker".into())
            .spawn(move || owned.run())
            .map_err(internal)?;
        *runtime.worker.lock().map_err(internal)? = Some(worker);
        Ok(runtime)
    }
    pub fn shutdown(&self) {
        if let Ok(previews) = self.previews.lock() {
            for preview in previews.values() {
                preview.cancel();
            }
        }
        if let Ok(mut control) = self.control.lock() {
            control.stop = true;
            if let Some((_, token)) = &control.active {
                token.cancel();
            }
            self.changed.notify_all();
        }
        if let Ok(mut worker) = self.worker.lock() {
            if let Some(worker) = worker.take() {
                let _ = worker.join();
            }
        }
        // Drop outside collection locks: the final reference may call an OS
        // provider. Unretired durable leases deliberately preserve their grants.
        let assets = self
            .assets
            .lock()
            .map(|mut assets| std::mem::take(&mut *assets))
            .unwrap_or_default();
        drop(assets);
        let jobs = self
            .job_access
            .lock()
            .map(|mut jobs| std::mem::take(&mut *jobs))
            .unwrap_or_default();
        drop(jobs);
    }

    fn restore_source_access(&self) -> Result<(), EngineError> {
        let mut retained = Vec::new();
        for record in self.store.list()? {
            if matches!(record.state, JobState::Completed | JobState::Canceled) {
                continue;
            }
            let request = self.store.request(&record.id)?;
            let (access, _availability) =
                self.source_access.claim(&request.source.locator, true)?;
            // A revoked grant must fail the affected operation, not hide Jobs
            // or prevent unrelated requests from being restored.
            retained.push(request.source.locator);
            self.job_access
                .lock()
                .map_err(internal)?
                .insert(record.id, access);
        }
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.provider.reconcile_source_access(retained);
        }));
        Ok(())
    }
    fn release_job_access(&self, id: &str) {
        let access = self
            .job_access
            .lock()
            .ok()
            .and_then(|mut jobs| jobs.remove(id));
        if let Some(access) = access {
            access
                .retired
                .store(true, std::sync::atomic::Ordering::SeqCst);
            drop(access);
        }
    }
    pub fn observe(&self, observer: Box<dyn JobObserver>) {
        if let Ok(mut current) = self.observer.lock() {
            *current = Some(Arc::from(observer));
        }
    }
    fn notify(&self) {
        let observer = self.observer.lock().ok().and_then(|value| value.clone());
        if let Some(observer) = observer {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                observer.on_jobs_changed()
            }));
        }
    }
    fn update(&self, record: &JobRecord) -> Result<(), EngineError> {
        let control = self.control.lock().map_err(internal)?;
        let canceled = control
            .active
            .as_ref()
            .is_some_and(|(id, token)| id == &record.id && token.is_cancelled())
            && record.state != JobState::Completed;
        let mut record = record.clone();
        if canceled {
            record.state = JobState::Canceling;
        }
        // Persist any acquired reservation even if cancellation arrived during
        // the native call, so its pending resource remains owned and cleanable.
        self.store.update(&record)?;
        drop(control);
        self.notify();
        if canceled {
            Err(EngineError::Cancelled)
        } else {
            Ok(())
        }
    }
    fn wake(&self) {
        if let Ok(mut control) = self.control.lock() {
            control.wake = true;
            self.changed.notify_one();
        }
    }
    fn fail_runtime(&self, error: &EngineError) {
        if let Ok(mut health) = self.health.lock() {
            *health = Some(error.to_string().chars().take(1000).collect());
        }
        if let Ok(mut control) = self.control.lock() {
            control.stop = true;
            if let Some((_, token)) = &control.active {
                token.cancel();
            }
            self.changed.notify_all();
        }
        self.notify();
    }
    pub fn runtime_error(&self) -> Option<String> {
        self.health.lock().ok().and_then(|value| value.clone())
    }
    pub fn inspect(
        &self,
        locator: String,
        name: String,
        operation: Operation,
        token: Arc<CancellationToken>,
    ) -> Result<InspectedSource, EngineError> {
        if locator.is_empty() || locator.len() > 1024 * 1024 || name.len() > 4096 {
            return Err(configuration::invalid(
                "The selected source identifier is invalid.",
            ));
        }
        let access = self.source_access.retain(&locator)?;
        let opened = self.provider.open_source(locator.clone())?;
        let info = media::inspect(opened, self.provider.clone(), token)?;
        processing::validate_operation(&info, operation)?;
        if configuration::source_catalog(&info, operation).is_empty() {
            return Err(EngineError::UnsupportedMedia{detail:"None of the currently available converter formats can preserve this source's streams and chapters. The original is unchanged.".into()});
        }
        let source = InspectedSource {
            id: uuid::Uuid::new_v4().to_string(),
            locator,
            display_name: name,
            info,
        };
        let mut assets = self.assets.lock().map_err(internal)?;
        if assets.len() >= 32 {
            return Err(EngineError::Platform {
                detail: "Close an unused media workspace before opening another source.".into(),
            });
        }
        assets.insert(
            source.id.clone(),
            SourceAsset {
                source: source.clone(),
                _access: access,
            },
        );
        Ok(source)
    }
    pub fn release(&self, id: &str) {
        let removed = self
            .assets
            .lock()
            .ok()
            .and_then(|mut assets| assets.remove(id));
        self.release_preview(id);
        drop(removed);
    }
    fn asset(&self, id: &str) -> Result<SourceAsset, EngineError> {
        self.assets
            .lock()
            .map_err(internal)?
            .get(id)
            .cloned()
            .ok_or_else(|| configuration::invalid("Select and inspect a source before continuing."))
    }
    pub fn source_catalog(
        &self,
        id: &str,
        operation: Operation,
    ) -> Result<Vec<crate::FormatOption>, EngineError> {
        Ok(configuration::source_catalog(
            &self.asset(id)?.info,
            operation,
        ))
    }
    pub fn defaults(
        &self,
        id: &str,
        operation: Operation,
    ) -> Result<ConversionConfig, EngineError> {
        let source = self.asset(id)?;
        Ok(configuration::default_config(
            operation,
            &source.info,
            &source.display_name,
        ))
    }
    pub fn assess(&self, id: &str, config: &ConversionConfig) -> Result<(), EngineError> {
        processing::plan(&self.asset(id)?.info, config).map(|_| ())
    }
    pub fn choices(
        &self,
        id: &str,
        config: &ConversionConfig,
    ) -> Result<crate::ConfigurationChoices, EngineError> {
        Ok(configuration::choices(&self.asset(id)?.info, config))
    }
    pub fn submit(
        &self,
        id: &str,
        key: String,
        config: ConversionConfig,
        token: Arc<CancellationToken>,
    ) -> Result<JobRecord, EngineError> {
        let _submit = self.submission.lock().map_err(internal)?;
        self.submit_asset(self.asset(id)?, key, config, token)
    }

    /// Callers hold submission ownership through acceptance. A transient asset
    /// allows an explicit target retry to reuse inspected source facts without
    /// adding a hidden workspace or mutating the old immutable request.
    fn submit_asset(
        &self,
        source: SourceAsset,
        key: String,
        config: ConversionConfig,
        token: Arc<CancellationToken>,
    ) -> Result<JobRecord, EngineError> {
        if key.len() < 16 || key.len() > 128 {
            return Err(configuration::invalid(
                "The submission identifier is invalid.",
            ));
        }
        if let Some(detail) = self.runtime_error() {
            return Err(EngineError::Storage{detail:format!("The job worker stopped safely: {detail}. Restart the application after resolving storage access.")});
        }
        let plan = processing::plan(&source.info, &config)?;
        if let Some((record, previous)) = self.store.by_submission(&key)? {
            if previous.source.info.source_identity != source.info.source_identity
                || serde_json::to_string(&previous.plan.config).map_err(internal)?
                    != serde_json::to_string(&config).map_err(internal)?
            {
                return Err(configuration::invalid(
                    "This submission identifier already belongs to a different accepted request.",
                ));
            }
            return Ok(record);
        }
        let reopened = self.provider.open_source(source.locator.clone())?;
        let input = Input::open(reopened, self.provider.clone(), token.clone())?;
        if input.source_identity()? != source.info.source_identity {
            return Err(EngineError::SourceAccess {
                detail: "The source changed after inspection. Select it again before submitting."
                    .into(),
            });
        }
        drop(input);
        if token.is_cancelled() {
            return Err(EngineError::Cancelled);
        }
        let record = JobRecord {
            id: uuid::Uuid::new_v4().to_string(),
            source_name: source.display_name.clone(),
            file_name: format!("{}.{}", config.file_name, plan.output()?.extension),
            state: JobState::Queued,
            processed_us: 0,
            duration_us: source.info.duration_us,
            error: None,
            error_category: None,
            created_ms: now_ms(),
            finished_ms: None,
            source_size: source.info.byte_size,
            verified: false,
            verified_size: None,
            verified_hash: None,
            reservation: None,
            output: None,
            keep_target_miss: false,
        };
        let (access, availability) = self.source_access.claim(&source.locator, true)?;
        if let Err(error) = availability {
            access
                .retired
                .store(true, std::sync::atomic::Ordering::SeqCst);
            return Err(error);
        }
        match self.job_access.lock() {
            Ok(mut jobs) => {
                jobs.insert(record.id.clone(), access);
            }
            Err(error) => {
                access
                    .retired
                    .store(true, std::sync::atomic::Ordering::SeqCst);
                return Err(internal(error));
            }
        }
        let insertion = self.store.insert(
            &record,
            &AcceptedRequest {
                version: CURRENT_ACCEPTED_REQUEST_VERSION,
                submission_key: key,
                source: source.source.clone(),
                plan,
            },
        );
        if let Err(error) = insertion {
            self.release_job_access(&record.id);
            return Err(error);
        }
        self.notify();
        self.wake();
        Ok(record)
    }
    pub fn jobs(&self) -> Result<Vec<JobRecord>, EngineError> {
        self.store.list()
    }

    pub fn keep_target_output(&self, id: &str) -> Result<(), EngineError> {
        let _submit = self.submission.lock().map_err(internal)?;
        let control = self.control.lock().map_err(internal)?;
        let mut record = self.store.get(id)?;
        // A repeated acknowledgement must never schedule another worker.
        if record.keep_target_miss
            && matches!(
                record.state,
                JobState::Queued
                    | JobState::Preparing
                    | JobState::Processing
                    | JobState::Finalizing
                    | JobState::Validating
                    | JobState::Publishing
                    | JobState::Completed
            )
        {
            return Ok(());
        }
        if record.state != JobState::TargetNotMet {
            return Err(configuration::invalid("Only an output awaiting a target-size decision can be kept. Use Save Again for a failed publication."));
        }
        if control
            .active
            .as_ref()
            .is_some_and(|(active, _)| active == id)
        {
            return Err(configuration::invalid(
                "Wait for the processing worker to stop before keeping this output.",
            ));
        }
        let request = self.store.request(id)?;
        if !target_requires_decision(&record, &request.plan) || record.verified_hash.is_none() {
            return Err(EngineError::Recovery { detail: "This job has no retained verified target-size result to keep. Its resources have been preserved.".into() });
        }
        record.keep_target_miss = true;
        record.state = JobState::Queued;
        record.error = None;
        record.error_category = None;
        record.finished_ms = None;
        self.store.update(&record)?;
        drop(control);
        self.notify();
        self.wake();
        Ok(())
    }

    pub fn resubmit_target(
        &self,
        id: &str,
        new_target: Option<TargetSize>,
        key: String,
        token: Arc<CancellationToken>,
    ) -> Result<JobRecord, EngineError> {
        let _submit = self.submission.lock().map_err(internal)?;
        if key.len() < 16 || key.len() > 128 {
            return Err(configuration::invalid(
                "The submission identifier is invalid.",
            ));
        }
        let previous = self.store.request(id)?;
        let mut config = previous.plan.config.clone();
        if let Some(target) = new_target {
            target.bytes()?;
            let compression = config.compression.get_or_insert_with(Default::default);
            compression.reduce_file_size = true;
            compression.mode = CompressionMode::TargetSize;
            compression.target = Some(target);
            compression.custom_video = None;
            compression.custom_audio = None;
        }
        // A retry of an accepted submission returns that same new job even if
        // the user subsequently kept or discarded the original result.
        if let Some((record, accepted)) = self.store.by_submission(&key)? {
            if accepted.source.info.source_identity != previous.source.info.source_identity
                || serde_json::to_string(&accepted.plan.config).map_err(internal)?
                    != serde_json::to_string(&config).map_err(internal)?
            {
                return Err(configuration::invalid(
                    "This submission identifier already belongs to a different accepted request.",
                ));
            }
            return Ok(record);
        }
        let control = self.control.lock().map_err(internal)?;
        let record = self.store.get(id)?;
        if record.state != JobState::TargetNotMet {
            return Err(configuration::invalid(
                "Only an output awaiting a target-size decision can be adjusted or retried.",
            ));
        }
        if !target_requires_decision(&record, &previous.plan) || record.verified_hash.is_none() {
            return Err(EngineError::Recovery { detail: "This job has no retained verified target-size result to retry. Its resources have been preserved.".into() });
        }
        if control
            .active
            .as_ref()
            .is_some_and(|(active, _)| active == id)
        {
            return Err(configuration::invalid(
                "Wait for the processing worker to stop before retrying this target.",
            ));
        }
        drop(control);
        let access = self.source_access.retain(&previous.source.locator)?;
        self.submit_asset(
            SourceAsset {
                source: previous.source,
                _access: access,
            },
            key,
            config,
            token,
        )
    }

    pub fn discard_target_output(&self, id: &str) -> Result<(), EngineError> {
        let _submit = self.submission.lock().map_err(internal)?;
        let control = self.control.lock().map_err(internal)?;
        let mut record = self.store.get(id)?;
        if record.state == JobState::Canceled {
            return Ok(());
        }
        if record.state != JobState::TargetNotMet {
            return Err(configuration::invalid(
                "Only an output awaiting a target-size decision can be discarded here.",
            ));
        }
        if control
            .active
            .as_ref()
            .is_some_and(|(active, _)| active == id)
        {
            return Err(configuration::invalid(
                "Wait for worker termination before discarding its output.",
            ));
        }
        record.state = JobState::Canceled;
        record.error = None;
        record.error_category = None;
        record.finished_ms = Some(now_ms());
        self.store.update(&record)?;
        drop(control);
        self.cleanup(&record);
        self.release_job_access(id);
        self.notify();
        Ok(())
    }

    pub fn cancel(&self, id: &str) -> Result<(), EngineError> {
        let control = self.control.lock().map_err(internal)?;
        let mut record = self.store.get(id)?;
        if record.state.terminal() {
            return Ok(());
        }
        if let Some((active, token)) = &control.active {
            if active == id {
                token.cancel();
                record.state = JobState::Canceling;
                self.store.update(&record)?;
                drop(control);
                self.notify();
                return Ok(());
            }
        }
        record.state = JobState::Canceled;
        record.error = None;
        record.error_category = None;
        record.finished_ms = Some(now_ms());
        self.store.update(&record)?;
        drop(control);
        self.cleanup(&record);
        self.release_job_access(id);
        self.notify();
        Ok(())
    }
    pub fn restart(&self, id: &str) -> Result<(), EngineError> {
        self.requeue(id, false)
    }
    pub fn retry_publication(&self, id: &str) -> Result<(), EngineError> {
        self.requeue(id, true)
    }
    fn requeue(&self, id: &str, reuse_verified: bool) -> Result<(), EngineError> {
        let control = self.control.lock().map_err(internal)?;
        let mut record = self.store.get(id)?;
        if !matches!(record.state, JobState::Interrupted | JobState::Failed) {
            return Err(configuration::invalid(
                "Only failed or interrupted jobs can restart.",
            ));
        }
        if control
            .active
            .as_ref()
            .is_some_and(|(active, _)| active == id)
        {
            return Err(configuration::invalid(
                "Wait for the worker to stop before restarting.",
            ));
        }
        if record.verified && self.reconcile_publication(&mut record)? {
            self.store.update(&record)?;
            drop(control);
            self.release_job_access(id);
            self.notify();
            return Ok(());
        }
        if reuse_verified && !record.verified {
            return Err(configuration::invalid(
                "This job has no verified output to save. Restart processing instead.",
            ));
        }
        if !reuse_verified {
            self.cleanup(&record);
            record.reservation = None;
            record.processed_us = 0;
            record.verified = false;
            record.verified_size = None;
            record.verified_hash = None;
            record.keep_target_miss = false;
        }
        record.state = JobState::Queued;
        record.error = None;
        record.error_category = None;
        record.finished_ms = None;
        self.store.update(&record)?;
        drop(control);
        self.notify();
        self.wake();
        Ok(())
    }
    pub fn dismiss(&self, id: &str) -> Result<(), EngineError> {
        let control = self.control.lock().map_err(internal)?;
        let record = self.store.get(id)?;
        if !record.state.terminal() && record.state != JobState::Interrupted {
            return Err(configuration::invalid(
                "Cancel the unfinished job before dismissing it.",
            ));
        }
        if control
            .active
            .as_ref()
            .is_some_and(|(active, _)| active == id)
        {
            return Err(configuration::invalid(
                "Wait for worker termination before cleanup.",
            ));
        }
        self.cleanup(&record);
        self.store.remove(id)?;
        drop(control);
        self.release_job_access(id);
        self.notify();
        Ok(())
    }
    fn work_path(&self, record: &JobRecord) -> Result<PathBuf, EngineError> {
        let request = self.store.request(&record.id)?;
        let output = request.plan.output()?;
        Ok(self
            .root
            .join("jobs")
            .join(&record.id)
            .join(format!("output.{}", output.extension)))
    }
    fn cleanup(&self, record: &JobRecord) {
        if let Some(reservation) = &record.reservation {
            if record.output.is_none() {
                self.provider.discard_reservation(reservation.clone());
            }
        }
        let directory = self.root.join("jobs").join(&record.id);
        if uuid::Uuid::parse_str(&record.id).is_ok()
            && fs::read_to_string(directory.join(".velmux-job"))
                .ok()
                .as_deref()
                == Some(&record.id)
        {
            let _ = fs::remove_dir_all(directory);
        }
    }
    fn prepare_workdir(&self, record: &JobRecord) -> Result<(), EngineError> {
        let parent = self.root.join("jobs");
        fs::create_dir_all(&parent).map_err(storage)?;
        if parent
            .symlink_metadata()
            .map_err(storage)?
            .file_type()
            .is_symlink()
        {
            return Err(storage(
                "The working directory is not owned private storage.",
            ));
        }
        let directory = parent.join(&record.id);
        match fs::create_dir(&directory) {
            Ok(()) => {
                use std::io::Write;
                let mut marker = fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(directory.join(".velmux-job"))
                    .map_err(storage)?;
                marker.write_all(record.id.as_bytes()).map_err(storage)?;
                marker.sync_all().map_err(storage)?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                if directory
                    .symlink_metadata()
                    .map_err(storage)?
                    .file_type()
                    .is_symlink()
                    || fs::read_to_string(directory.join(".velmux-job"))
                        .ok()
                        .as_deref()
                        != Some(&record.id)
                {
                    return Err(storage("An unidentified directory occupies this job's working location. Its contents have been preserved."));
                }
            }
            Err(error) => return Err(storage(error)),
        }
        Ok(())
    }
    fn restore(&self) -> Result<(), EngineError> {
        for mut record in self.store.list()? {
            if !matches!(
                record.state,
                JobState::Completed | JobState::Canceled | JobState::Canceling | JobState::Paused
            ) {
                let request = self.store.request(&record.id)?;
                // A crash after verification but before the target decision
                // must not turn a missed target into an automatic save or a
                // failed-record retention cleanup. No worker survives startup.
                if self.hold_target_miss(&mut record, &request.plan)? {
                    continue;
                }
            }
            if record.verified
                && record.reservation.is_some()
                && record.state != JobState::Completed
            {
                match self.reconcile_publication(&mut record) {
                    Ok(true) => {
                        self.store.update(&record)?;
                        self.cleanup(&record);
                        self.release_job_access(&record.id);
                        continue;
                    }
                    Ok(false) => {}
                    Err(error) => {
                        record.state = JobState::Failed;
                        record.error = Some(error.to_string());
                        record.error_category = Some(category(&error).into());
                        record.finished_ms = Some(now_ms());
                        self.store.update(&record)?;
                        continue;
                    }
                }
            }
            if record.state == JobState::Canceling {
                record.state = JobState::Canceled;
                record.finished_ms = Some(now_ms());
                self.store.update(&record)?;
                self.cleanup(&record);
                self.release_job_access(&record.id);
                continue;
            }
            if record.state.active() {
                record.state = JobState::Interrupted;
                record.error=Some("Processing was interrupted. Restart begins from the start unless a verified output is ready to save.".into());
                record.error_category = Some("interrupted".into());
                self.store.update(&record)?;
            }
            let retention = if matches!(record.state, JobState::Completed | JobState::Canceled) {
                Some(3_600_000)
            } else if record.state == JobState::Failed {
                Some(86_400_000)
            } else {
                None
            };
            if retention.is_some_and(|limit| {
                record
                    .finished_ms
                    .is_some_and(|time| now_ms().saturating_sub(time) > limit)
            }) {
                self.cleanup(&record);
                self.store.remove(&record.id)?;
                self.release_job_access(&record.id);
            }
        }
        Ok(())
    }
    fn run(self: Arc<Self>) {
        loop {
            let mut control = match self.control.lock() {
                Ok(value) => value,
                Err(_) => return,
            };
            if control.stop {
                return;
            }
            let next = match self.store.next_queued() {
                Ok(value) => value,
                Err(error) => {
                    drop(control);
                    self.fail_runtime(&error);
                    return;
                }
            };
            let Some(id) = next else {
                control.wake = false;
                let _guard = self.changed.wait_while(control, |c| !c.stop && !c.wake);
                continue;
            };
            let token = CancellationToken::new();
            control.active = Some((id.clone(), token.clone()));
            let mut record = match self.store.get(&id) {
                Ok(value) => value,
                Err(error) => {
                    drop(control);
                    self.fail_runtime(&error);
                    return;
                }
            };
            record.state = JobState::Preparing;
            if let Err(error) = self.store.update(&record) {
                drop(control);
                self.fail_runtime(&error);
                return;
            }
            drop(control);
            self.notify();
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                self.process(&id, token.clone())
            }))
            .unwrap_or_else(|_| {
                Err(EngineError::Internal {
                    detail: "The processing worker encountered an unexpected internal failure."
                        .into(),
                })
            });
            // The processing call and all FFmpeg destructors have returned before
            // terminal cancellation or any scratch cleanup becomes observable.
            let mut control = match self.control.lock() {
                Ok(value) => value,
                Err(_) => return,
            };
            let mut persistence_error = None;
            // Cancellation can arrive after a target decision was persisted
            // but before the worker acknowledges return. Complete that cancel
            // only now, after every processing resource has been released.
            let result = if result.is_ok()
                && token.is_cancelled()
                && self
                    .store
                    .get(&id)
                    .is_ok_and(|record| record.state != JobState::Completed)
            {
                Err(EngineError::Cancelled)
            } else {
                result
            };
            if let Err(error) = result {
                if let Ok(mut record) = self.store.get(&id) {
                    record.state = if control.stop {
                        JobState::Interrupted
                    } else if token.is_cancelled() || matches!(error, EngineError::Cancelled) {
                        JobState::Canceled
                    } else {
                        JobState::Failed
                    };
                    record.error = if record.state == JobState::Canceled {
                        None
                    } else {
                        Some(error.to_string().chars().take(1000).collect())
                    };
                    record.error_category = Some(category(&error).into());
                    record.finished_ms = Some(now_ms());
                    if let Err(error) = self.store.update(&record) {
                        persistence_error = Some(error);
                    }
                    if record.state == JobState::Canceled {
                        self.cleanup(&record);
                    }
                }
            }
            control.active = None;
            drop(control);
            if self.store.get(&id).is_ok_and(|record| {
                matches!(record.state, JobState::Completed | JobState::Canceled)
            }) {
                self.release_job_access(&id);
            }
            self.notify();
            if let Some(error) = persistence_error {
                self.fail_runtime(&error);
                return;
            }
        }
    }
    fn process(&self, id: &str, token: Arc<CancellationToken>) -> Result<(), EngineError> {
        let request = self.store.request(id)?;
        let mut record = self.store.get(id)?;
        let path = self.work_path(&record)?;
        if record.verified {
            if !target_requires_decision(&record, &request.plan)
                && self.reconcile_publication(&mut record)?
            {
                self.update(&record)?;
                self.cleanup(&record);
                return Ok(());
            }
            let (size, hash) = verified_identity(&path, &token)?;
            if record.verified_size != Some(size) || record.verified_hash.as_deref() != Some(&hash)
            {
                return Err(EngineError::Recovery{detail:"The retained working output changed. It cannot be published as the previously verified result.".into()});
            }
            let output = media::inspect_file(&path, token.clone())?;
            processing::verify_output(&output, &request.source.info, &request.plan)?;
        }
        if !record.verified {
            self.prepare_workdir(&record)?;
            record.state = JobState::Processing;
            self.update(&record)?;
            let mut reported = Instant::now()
                .checked_sub(Duration::from_secs(1))
                .unwrap_or_else(Instant::now);
            processing::execute_conversion(
                self.provider.clone(),
                &request.source.locator,
                &request.source.info,
                &request.plan,
                &path,
                token.clone(),
                |event| {
                    let transition = !matches!(event, processing::ProcessingProgress::Timestamp(_));
                    if transition || reported.elapsed() >= Duration::from_millis(250) {
                        let mut latest = self.store.get(id)?;
                        match event {
                            processing::ProcessingProgress::Timestamp(position) => {
                                latest.processed_us = position
                            }
                            processing::ProcessingProgress::Finalizing => {
                                latest.state = JobState::Finalizing
                            }
                            processing::ProcessingProgress::Restarted => {
                                latest.processed_us = 0;
                                latest.state = JobState::Processing;
                            }
                            processing::ProcessingProgress::Validating => {
                                latest.state = JobState::Validating;
                            }
                        }
                        self.update(&latest)?;
                        reported = Instant::now();
                    }
                    Ok(())
                },
            )?;
            if token.is_cancelled() {
                return Err(EngineError::Cancelled);
            }
            record = self.store.get(id)?;
            // The processing owner has independently inspected and validated
            // the actual finalized output against this accepted request.
            let (size, hash) = verified_identity(&path, &token)?;
            record.verified = true;
            record.verified_size = Some(size);
            record.verified_hash = Some(hash);
            self.update(&record)?;
        }
        if token.is_cancelled() {
            return Err(EngineError::Cancelled);
        }
        let expected_size = record
            .verified_size
            .ok_or_else(|| internal("Missing verified output identity"))?;
        if expected_size == 0 {
            return Err(EngineError::InvalidMedia {
                detail: "The finalized working output is empty.".into(),
            });
        }
        // Never reserve or publish a target miss until an explicit keep
        // decision has been persisted. Retain verified work and source access.
        if self.hold_target_miss(&mut record, &request.plan)? {
            return Ok(());
        }
        if record.reservation.is_none() {
            let reservation = self.provider.reserve_output(
                record.id.clone(),
                record.file_name.clone(),
                request.plan.output()?.mime_type,
            )?;
            record.reservation = Some(reservation);
            self.update(&record)?;
        }
        if token.is_cancelled() {
            return Err(EngineError::Cancelled);
        }
        record.state = JobState::Publishing;
        self.update(&record)?;
        let published = self.provider.publish_output(
            record.reservation.clone().unwrap(),
            path.to_string_lossy().into_owned(),
            expected_size,
        );
        let output = match published {
            Ok(output) => output,
            Err(error) => {
                if self.reconcile_publication(&mut record)? {
                    self.update(&record)?;
                    self.cleanup(&record);
                    return Ok(());
                }
                return Err(error);
            }
        };
        if output.size != expected_size {
            return Err(EngineError::Storage{detail:"Publication returned a different output size. The verified working output is retained.".into()});
        }
        record.output = Some(output);
        record.state = JobState::Completed;
        record.finished_ms = Some(now_ms());
        record.error = None;
        record.error_category = None;
        self.update(&record)?;
        self.cleanup(&record);
        Ok(())
    }
    fn reconcile_publication(&self, record: &mut JobRecord) -> Result<bool, EngineError> {
        let request = self.store.request(&record.id)?;
        if target_requires_decision(record, &request.plan) {
            return Ok(false);
        }
        let Some(reservation) = &record.reservation else {
            return Ok(false);
        };
        let Some(output) = self.provider.publication_status(reservation.clone())? else {
            return Ok(false);
        };
        if record.verified_size != Some(output.size) || output.size == 0 {
            return Err(EngineError::Storage{detail:"An existing publication does not match the verified output size. The user file has been preserved.".into()});
        }
        record.output = Some(output);
        record.state = JobState::Completed;
        record.finished_ms = Some(now_ms());
        record.error = None;
        record.error_category = None;
        Ok(true)
    }

    fn hold_target_miss(&self, record: &mut JobRecord, plan: &Plan) -> Result<bool, EngineError> {
        if !target_requires_decision(record, plan) {
            return Ok(false);
        }
        record.state = JobState::TargetNotMet;
        record.error = Some(format!("The verified output is {} bytes, exceeding the requested {}-byte limit. Keep this output, adjust the target and retry, or discard it. Nothing has been published.", record.verified_size.unwrap(), plan.target_size_bytes.unwrap()));
        record.error_category = Some("target_not_met".into());
        record.finished_ms = None;
        self.update(record)?;
        Ok(true)
    }
    pub fn appearance(&self) -> Result<String, EngineError> {
        Ok(self
            .store
            .setting("appearance")?
            .unwrap_or_else(|| "system".into()))
    }
    pub fn set_appearance(&self, value: &str) -> Result<(), EngineError> {
        if !["system", "light", "dark"].contains(&value) {
            return Err(configuration::invalid("Unknown appearance preference."));
        }
        self.store.set_setting("appearance", value)
    }
}

fn target_requires_decision(record: &JobRecord, plan: &Plan) -> bool {
    record.verified
        && !record.keep_target_miss
        && record
            .verified_size
            .zip(plan.target_size_bytes)
            .is_some_and(|(actual, target)| actual > target)
}

fn verified_identity(
    path: &std::path::Path,
    token: &CancellationToken,
) -> Result<(u64, String), EngineError> {
    let mut file = fs::File::open(path).map_err(storage)?;
    let mut digest = Sha256::new();
    let mut size = 0u64;
    let mut bytes = [0u8; 64 * 1024];
    loop {
        if token.is_cancelled() {
            return Err(EngineError::Cancelled);
        }
        let count = file.read(&mut bytes).map_err(storage)?;
        if count == 0 {
            break;
        }
        size = size
            .checked_add(count as u64)
            .ok_or_else(|| storage("The working output is too large"))?;
        digest.update(&bytes[..count]);
    }
    Ok((size, format!("{:x}", digest.finalize())))
}
