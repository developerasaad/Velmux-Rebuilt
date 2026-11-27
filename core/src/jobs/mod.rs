mod store;

use crate::{
    configuration::{self, ConversionConfig, Operation},
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
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct AcceptedRequest {
    submission_key: String,
    source: InspectedSource,
    plan: Plan,
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
pub(crate) struct Runtime {
    root: PathBuf,
    pub provider: Arc<dyn MediaProvider>,
    store: Store,
    assets: Mutex<HashMap<String, InspectedSource>>,
    control: Mutex<Control>,
    changed: Condvar,
    observer: Mutex<Option<Arc<dyn JobObserver>>>,
    worker: Mutex<Option<JoinHandle<()>>>,
    submission: Mutex<()>,
    health: Mutex<Option<String>>,
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
        let runtime = Arc::new(Self {
            store: Store::open(&root)?,
            root,
            provider: Arc::from(provider),
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
        });
        runtime.restore()?;
        let owned = runtime.clone();
        let worker = thread::Builder::new()
            .name("velmux-media-worker".into())
            .spawn(move || owned.run())
            .map_err(internal)?;
        *runtime.worker.lock().map_err(internal)? = Some(worker);
        Ok(runtime)
    }
    pub fn shutdown(&self) {
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
        let opened = self.provider.open_source(locator.clone())?;
        let info = media::inspect(opened, self.provider.clone(), token)?;
        processing::validate_operation(&info, operation)?;
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
        assets.insert(source.id.clone(), source.clone());
        Ok(source)
    }
    pub fn release(&self, id: &str) {
        if let Ok(mut assets) = self.assets.lock() {
            assets.remove(id);
        }
    }
    fn asset(&self, id: &str) -> Result<InspectedSource, EngineError> {
        self.assets
            .lock()
            .map_err(internal)?
            .get(id)
            .cloned()
            .ok_or_else(|| configuration::invalid("Select and inspect a source before continuing."))
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
    pub fn submit(
        &self,
        id: &str,
        key: String,
        config: ConversionConfig,
        token: Arc<CancellationToken>,
    ) -> Result<JobRecord, EngineError> {
        if key.len() < 16 || key.len() > 128 {
            return Err(configuration::invalid(
                "The submission identifier is invalid.",
            ));
        }
        let _submit = self.submission.lock().map_err(internal)?;
        if let Some(detail) = self.runtime_error() {
            return Err(EngineError::Storage{detail:format!("The job worker stopped safely: {detail}. Restart the application after resolving storage access.")});
        }
        let source = self.asset(id)?;
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
            file_name: format!("{}.{}", config.file_name, config.format.extension()),
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
        };
        self.store.insert(
            &record,
            &AcceptedRequest {
                submission_key: key,
                source,
                plan,
            },
        )?;
        self.notify();
        self.wake();
        Ok(record)
    }
    pub fn jobs(&self) -> Result<Vec<JobRecord>, EngineError> {
        self.store.list()
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
        record.finished_ms = Some(now_ms());
        self.store.update(&record)?;
        drop(control);
        self.cleanup(&record);
        self.notify();
        Ok(())
    }
    pub fn restart(&self, id: &str) -> Result<(), EngineError> {
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
        if !record.verified {
            self.cleanup(&record);
            record.reservation = None;
            record.processed_us = 0;
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
        self.notify();
        Ok(())
    }
    fn work_path(&self, record: &JobRecord) -> Result<PathBuf, EngineError> {
        let request = self.store.request(&record.id)?;
        Ok(self
            .root
            .join("jobs")
            .join(&record.id)
            .join(format!("output.{}", request.plan.config.format.extension())))
    }
    fn cleanup(&self, record: &JobRecord) {
        if let Some(reservation) = &record.reservation {
            if record.output.is_none() {
                self.provider.discard_reservation(reservation.clone());
            }
        }
        let directory = self.root.join("jobs").join(&record.id);
        if uuid::Uuid::parse_str(&record.id).is_ok() {
            let _ = fs::remove_dir_all(directory);
        }
    }
    fn restore(&self) -> Result<(), EngineError> {
        for mut record in self.store.list()? {
            if record.verified
                && record.reservation.is_some()
                && record.state != JobState::Completed
            {
                match self.reconcile_publication(&mut record) {
                    Ok(true) => {
                        self.store.update(&record)?;
                        self.cleanup(&record);
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
            if self.reconcile_publication(&mut record)? {
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
            let opened = self.provider.open_source(request.source.locator.clone())?;
            let input = Input::open(opened, self.provider.clone(), token.clone())?;
            if input.source_identity()? != request.source.info.source_identity {
                return Err(EngineError::SourceAccess {
                    detail: "The original source changed. Re-select it before processing.".into(),
                });
            }
            fs::create_dir_all(path.parent().unwrap()).map_err(storage)?;
            record.state = JobState::Processing;
            self.update(&record)?;
            let mut reported = Instant::now()
                .checked_sub(Duration::from_secs(1))
                .unwrap_or_else(Instant::now);
            processing::convert(
                input,
                &request.source.info,
                &request.plan,
                &path,
                token.clone(),
                |position| {
                    if reported.elapsed() >= Duration::from_millis(250) {
                        if let Ok(mut latest) = self.store.get(id) {
                            if latest.state == JobState::Processing {
                                latest.processed_us = position;
                                let _ = self.update(&latest);
                            }
                        }
                        reported = Instant::now();
                    }
                },
            )?;
            if token.is_cancelled() {
                return Err(EngineError::Cancelled);
            }
            record = self.store.get(id)?;
            record.state = JobState::Finalizing;
            self.update(&record)?;
            record.state = JobState::Validating;
            self.update(&record)?;
            let output = media::inspect_file(&path, token.clone())?;
            processing::verify_output(&output, &request.source.info, &request.plan)?;
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
        if record.reservation.is_none() {
            let reservation = self.provider.reserve_output(
                record.id.clone(),
                record.file_name.clone(),
                request.plan.config.format.mime_type(),
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
