//! Real media integration. CLI fixture generation and independent output checks
//! live outside the engine; this provider only implements host filesystem facilities.
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::fd::IntoRawFd,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Condvar, Mutex, Weak,
    },
    thread,
    time::{Duration, Instant},
};
use velmux_core::{
    AudioCodec, AudioLayout, AudioPrecision, AudioRateControl, CancellationToken, CompressionMode,
    CompressionSettings, ConversionConfig, Engine, EngineError, FrameRate, InspectedSource,
    JobObserver, JobRecord, JobState, MediaProvider, NativeProvider, OpenedSource, Operation,
    OutputFormat, OutputReservation, PublishedOutput, StreamKind, TargetSize, TargetSizeUnit,
    VideoCodec, VideoRateControl, VideoSize,
};

fn repository() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}

fn fixture(name: &str) -> PathBuf {
    let path = repository().join("build/media-fixtures").join(name);
    assert!(
        path.is_file(),
        "Run scripts/ci/prepare-media-fixtures.sh first: {}",
        path.display()
    );
    path
}

fn storage(detail: impl ToString) -> EngineError {
    EngineError::Storage {
        detail: detail.to_string(),
    }
}

fn digest(path: &Path) -> String {
    let mut file = File::open(path).unwrap();
    let mut hash = Sha256::new();
    let mut bytes = [0u8; 64 * 1024];
    loop {
        let count = file.read(&mut bytes).unwrap();
        if count == 0 {
            break;
        }
        hash.update(&bytes[..count]);
    }
    format!("{:x}", hash.finalize())
}

#[derive(Default)]
struct Gate {
    state: Mutex<(bool, bool)>,
    changed: Condvar,
}
impl Gate {
    fn enter(&self) {
        let mut state = self.state.lock().unwrap();
        state.0 = true;
        self.changed.notify_all();
        while !state.1 {
            let (next, timeout) = self
                .changed
                .wait_timeout(state, Duration::from_secs(30))
                .unwrap();
            state = next;
            assert!(
                !timeout.timed_out(),
                "A native provider gate was not released"
            );
        }
    }
    fn wait_entered(&self) {
        let state = self.state.lock().unwrap();
        let (state, timeout) = self
            .changed
            .wait_timeout_while(state, Duration::from_secs(15), |state| !state.0)
            .unwrap();
        assert!(
            state.0 && !timeout.timed_out(),
            "The real worker did not reach its native provider"
        );
    }
    fn release(&self) {
        self.state.lock().unwrap().1 = true;
        self.changed.notify_all();
    }
}

struct ProviderState {
    root: PathBuf,
    source_gate: Gate,
    publication_gate: Gate,
    preview_gate: Gate,
    access_release_gate: Gate,
    block_source: AtomicBool,
    block_publication: AtomicBool,
    block_preview: AtomicBool,
    block_access_release: AtomicBool,
    fail_publications: AtomicUsize,
    fail_after_publish: AtomicBool,
    open_calls: AtomicUsize,
    worker_sources: AtomicUsize,
    maximum_worker_sources: AtomicUsize,
    publish_calls: AtomicUsize,
    leases: Mutex<HashMap<String, bool>>,
    lease_locators: Mutex<HashMap<String, String>>,
    retained_sources: Mutex<HashSet<String>>,
    access_events: Mutex<Vec<(String, String)>>,
    publication_lock: Mutex<()>,
}
impl ProviderState {
    fn new(root: PathBuf) -> Arc<Self> {
        Arc::new(Self {
            root,
            source_gate: Gate::default(),
            publication_gate: Gate::default(),
            preview_gate: Gate::default(),
            access_release_gate: Gate::default(),
            block_source: AtomicBool::new(false),
            block_publication: AtomicBool::new(false),
            block_preview: AtomicBool::new(false),
            block_access_release: AtomicBool::new(false),
            fail_publications: AtomicUsize::new(0),
            fail_after_publish: AtomicBool::new(false),
            open_calls: AtomicUsize::new(0),
            worker_sources: AtomicUsize::new(0),
            maximum_worker_sources: AtomicUsize::new(0),
            publish_calls: AtomicUsize::new(0),
            leases: Mutex::new(HashMap::new()),
            lease_locators: Mutex::new(HashMap::new()),
            retained_sources: Mutex::new(HashSet::new()),
            access_events: Mutex::new(Vec::new()),
            publication_lock: Mutex::new(()),
        })
    }
    fn receipt(&self, reservation: &OutputReservation) -> PathBuf {
        self.root.join("receipts").join(format!(
            "{}.json",
            Path::new(&reservation.locator)
                .file_name()
                .unwrap()
                .to_str()
                .unwrap()
        ))
    }
    fn published(
        &self,
        reservation: &OutputReservation,
    ) -> Result<Option<PublishedOutput>, EngineError> {
        let receipt = self.receipt(reservation);
        if !receipt.exists() {
            return Ok(None);
        }
        let value: Value =
            serde_json::from_slice(&fs::read(receipt).map_err(storage)?).map_err(storage)?;
        let destination = Path::new(&reservation.locator);
        let size = fs::metadata(destination)
            .map_err(|_| storage("The published file was moved outside Velmux"))?
            .len();
        if value["size"].as_u64() != Some(size)
            || value["sha256"].as_str() != Some(digest(destination).as_str())
        {
            return Err(storage("The published file changed outside Velmux"));
        }
        Ok(Some(PublishedOutput {
            locator: reservation.locator.clone(),
            display_name: reservation.display_name.clone(),
            size,
        }))
    }
}

#[derive(Clone)]
struct FilesystemProvider(Arc<ProviderState>);
impl MediaProvider for FilesystemProvider {
    fn cache_root(&self) -> String {
        self.0.root.join("cache").to_string_lossy().into_owned()
    }
    fn output_location(&self) -> String {
        self.0.root.join("published").to_string_lossy().into_owned()
    }
    fn retain_source_access(&self, locator: String) -> Result<(), EngineError> {
        File::open(&locator).map_err(|error| EngineError::SourceAccess {
            detail: error.to_string(),
        })?;
        self.0
            .retained_sources
            .lock()
            .unwrap()
            .insert(locator.clone());
        self.0
            .access_events
            .lock()
            .unwrap()
            .push(("retain".into(), locator));
        Ok(())
    }
    fn release_source_access(&self, locator: String) {
        if self.0.block_access_release.load(Ordering::SeqCst) {
            self.0.access_release_gate.enter();
        }
        assert!(
            !self
                .0
                .lease_locators
                .lock()
                .unwrap()
                .values()
                .any(|value| value == &locator),
            "Durable access was released before an opened descriptor lease returned"
        );
        self.0.retained_sources.lock().unwrap().remove(&locator);
        self.0
            .access_events
            .lock()
            .unwrap()
            .push(("release".into(), locator));
    }
    fn reconcile_source_access(&self, retained_locators: Vec<String>) {
        let retained: HashSet<_> = retained_locators.into_iter().collect();
        self.0
            .retained_sources
            .lock()
            .unwrap()
            .retain(|locator| retained.contains(locator));
        self.0
            .access_events
            .lock()
            .unwrap()
            .push(("reconcile".into(), String::new()));
    }
    fn open_source(&self, locator: String) -> Result<OpenedSource, EngineError> {
        let file = File::open(&locator).map_err(|error| EngineError::SourceAccess {
            detail: error.to_string(),
        })?;
        let length = file.metadata().map_err(storage)?.len();
        self.0.open_calls.fetch_add(1, Ordering::SeqCst);
        let worker = thread::current().name() == Some("velmux-media-worker");
        if thread::current().name() == Some("velmux-preview-test")
            && self.0.block_preview.load(Ordering::SeqCst)
        {
            self.0.preview_gate.enter();
        }
        if worker {
            let active = self.0.worker_sources.fetch_add(1, Ordering::SeqCst) + 1;
            self.0
                .maximum_worker_sources
                .fetch_max(active, Ordering::SeqCst);
            if self.0.block_source.load(Ordering::SeqCst) {
                self.0.source_gate.enter();
            }
        }
        let lease = uuid::Uuid::new_v4().to_string();
        self.0
            .lease_locators
            .lock()
            .unwrap()
            .insert(lease.clone(), locator);
        self.0.leases.lock().unwrap().insert(lease.clone(), worker);
        Ok(OpenedSource {
            fd: file.into_raw_fd(),
            offset: 0,
            length: Some(length),
            lease_id: lease,
        })
    }
    fn release_source(&self, lease_id: String) {
        self.0.lease_locators.lock().unwrap().remove(&lease_id);
        if self.0.leases.lock().unwrap().remove(&lease_id) == Some(true) {
            self.0.worker_sources.fetch_sub(1, Ordering::SeqCst);
        }
    }
    fn reserve_output(
        &self,
        job_id: String,
        file_name: String,
        mime_type: String,
    ) -> Result<OutputReservation, EngineError> {
        assert_eq!(Path::new(&file_name).components().count(), 1);
        fs::create_dir_all(self.0.root.join("published")).map_err(storage)?;
        fs::create_dir_all(self.0.root.join("receipts")).map_err(storage)?;
        Ok(OutputReservation {
            locator: self
                .0
                .root
                .join("published")
                .join(format!("{job_id}-{file_name}"))
                .to_string_lossy()
                .into_owned(),
            display_name: file_name,
            mime_type,
        })
    }
    fn publish_output(
        &self,
        reservation: OutputReservation,
        working_path: String,
        expected_size: u64,
    ) -> Result<PublishedOutput, EngineError> {
        self.0.publish_calls.fetch_add(1, Ordering::SeqCst);
        if self.0.block_publication.load(Ordering::SeqCst) {
            self.0.publication_gate.enter();
        }
        let _lock = self.0.publication_lock.lock().unwrap();
        if let Some(output) = self.0.published(&reservation)? {
            return Ok(output);
        }
        if self
            .0
            .fail_publications
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |count| {
                count.checked_sub(1)
            })
            .is_ok()
        {
            return Err(storage(
                "Controlled destination failure; verified work must remain reusable",
            ));
        }
        assert!(Path::new(&working_path).starts_with(self.0.root.join("operational/jobs")));
        let mut source = File::open(&working_path).map_err(storage)?;
        assert_eq!(source.metadata().map_err(storage)?.len(), expected_size);
        let temporary = Path::new(&reservation.locator).with_extension("unpublished");
        let mut destination = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(storage)?;
        let copied = std::io::copy(&mut source, &mut destination).map_err(storage)?;
        destination.sync_all().map_err(storage)?;
        drop(destination);
        assert_eq!(copied, expected_size);
        // Exclusive reveal: never overwrite an unrelated fixture/user file.
        fs::hard_link(&temporary, &reservation.locator).map_err(storage)?;
        fs::remove_file(temporary).map_err(storage)?;
        let value = json!({"size": copied, "sha256": digest(Path::new(&reservation.locator))});
        fs::write(
            self.0.receipt(&reservation),
            serde_json::to_vec(&value).unwrap(),
        )
        .map_err(storage)?;
        if self.0.fail_after_publish.swap(false, Ordering::SeqCst) {
            // Emulate the iOS move and a failure after publication but before response.
            fs::remove_file(&working_path).map_err(storage)?;
            return Err(storage(
                "Controlled failure after a real atomic publication",
            ));
        }
        Ok(PublishedOutput {
            locator: reservation.locator,
            display_name: reservation.display_name,
            size: copied,
        })
    }
    fn publication_status(
        &self,
        reservation: OutputReservation,
    ) -> Result<Option<PublishedOutput>, EngineError> {
        let _lock = self.0.publication_lock.lock().unwrap();
        self.0.published(&reservation)
    }
    fn discard_reservation(&self, reservation: OutputReservation) {
        // Published files and receipts are never deleted by job dismissal.
        let _ = fs::remove_file(Path::new(&reservation.locator).with_extension("unpublished"));
    }
}

struct HostNativeProvider(PathBuf);
impl NativeProvider for HostNativeProvider {
    fn storage_root(&self) -> String {
        self.0.to_string_lossy().into_owned()
    }
    fn platform_name(&self) -> String {
        "host-media-integration".into()
    }
    fn on_diagnostic(&self, _sequence: u64) {}
}

struct Harness {
    root: PathBuf,
    engine: Option<Arc<Engine>>,
    provider: Arc<ProviderState>,
}
impl Harness {
    fn new(name: &str) -> Self {
        let root = repository().join("build/media-test-output").join(name);
        if root.exists() {
            fs::remove_dir_all(&root).unwrap();
        }
        fs::create_dir_all(root.join("operational")).unwrap();
        let provider = ProviderState::new(root.clone());
        let mut harness = Self {
            root,
            engine: None,
            provider,
        };
        harness.reopen();
        harness
    }
    fn engine(&self) -> &Arc<Engine> {
        self.engine.as_ref().unwrap()
    }
    fn close(&mut self) {
        drop(self.engine.take());
    }
    fn reopen(&mut self) {
        assert!(self.engine.is_none());
        let engine = Engine::new(Box::new(HostNativeProvider(self.root.join("operational"))));
        engine
            .initialize_media(Box::new(FilesystemProvider(self.provider.clone())))
            .unwrap();
        self.engine = Some(engine);
    }
    fn inspect(&self, name: &str, operation: Operation) -> InspectedSource {
        self.engine()
            .inspect_source(
                fixture(name).to_string_lossy().into_owned(),
                name.into(),
                operation,
                CancellationToken::new(),
            )
            .unwrap()
    }
    fn defaults(&self, source: &InspectedSource, operation: Operation) -> ConversionConfig {
        let mut config = self
            .engine()
            .default_configuration(source.id.clone(), operation)
            .unwrap();
        // Keep legacy regression controls explicit when product defaults evolve.
        config.compression = None;
        config.format = match operation {
            Operation::VideoConverter => OutputFormat::Mp4,
            Operation::AudioConverter => OutputFormat::M4a,
        };
        config
    }
    fn submit(&self, source: &InspectedSource, config: ConversionConfig) -> JobRecord {
        self.engine()
            .submit_conversion(
                source.id.clone(),
                uuid::Uuid::new_v4().to_string(),
                config,
                CancellationToken::new(),
            )
            .unwrap()
    }
    fn record(&self, id: &str) -> JobRecord {
        self.engine()
            .jobs()
            .unwrap()
            .into_iter()
            .find(|record| record.id == id)
            .unwrap()
    }
    fn wait(&self, id: &str, predicate: impl Fn(&JobRecord) -> bool) -> JobRecord {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let record = self.record(id);
            if predicate(&record) {
                return record;
            }
            assert!(
                Instant::now() < deadline,
                "Timed out waiting for job: {record:?}; health={:?}",
                self.engine().runtime_error()
            );
            thread::sleep(Duration::from_millis(10));
        }
    }
    fn finished(&self, id: &str) -> JobRecord {
        self.wait(id, |record| {
            matches!(
                record.state,
                JobState::Completed | JobState::Failed | JobState::Canceled
            )
        })
    }
    fn complete(&self, id: &str) -> JobRecord {
        let record = self.finished(id);
        assert_eq!(record.state, JobState::Completed, "{record:?}");
        assert!(
            record.verified && record.verified_size.is_some() && record.verified_hash.is_some()
        );
        assert_eq!(
            record.output.as_ref().unwrap().size,
            record.verified_size.unwrap()
        );
        record
    }
    fn work(&self, record: &JobRecord) -> PathBuf {
        self.root
            .join("operational/jobs")
            .join(&record.id)
            .join(format!(
                "output.{}",
                Path::new(&record.file_name)
                    .extension()
                    .unwrap()
                    .to_str()
                    .unwrap()
            ))
    }
    fn expect(&self, cases: Vec<Value>) {
        fs::write(
            self.root.join("expectations.json"),
            serde_json::to_vec_pretty(&cases).unwrap(),
        )
        .unwrap();
    }
    fn persist_record(&self, record: &JobRecord, state: &str) {
        assert!(
            self.engine.is_none(),
            "Only mutate fixtures while no runtime owns the store"
        );
        Connection::open(self.root.join("operational/operations.sqlite3"))
            .unwrap()
            .execute(
                "UPDATE jobs SET record=?1,state=?2 WHERE id=?3",
                params![serde_json::to_string(record).unwrap(), state, record.id],
            )
            .unwrap();
    }
}
impl Drop for Harness {
    fn drop(&mut self) {
        self.provider.source_gate.release();
        self.provider.publication_gate.release();
        self.provider.preview_gate.release();
        self.provider.access_release_gate.release();
        self.close();
    }
}

fn expectation(record: &JobRecord, audio_codec: &str, channels: u32, sample_rate: u32) -> Value {
    json!({"path": record.output.as_ref().unwrap().locator, "audio_codecs": [audio_codec],
        "channels": channels, "sample_rate": sample_rate, "duration": 2.0, "tones": true})
}

#[test]
fn real_video_encoding_copy_and_immutable_idempotent_submission() {
    let harness = Harness::new("video");
    let source = harness.inspect("video.mp4", Operation::VideoConverter);
    let original_hash = digest(&fixture("video.mp4"));
    let mut accepted = harness.defaults(&source, Operation::VideoConverter);
    accepted.video_codec = VideoCodec::H264;
    accepted.audio_codec = AudioCodec::Aac;
    accepted.frame_rate = FrameRate::Fps30;
    accepted.video_size = VideoSize::Hd720;
    accepted.file_name = "encoded_video".into();
    let key = uuid::Uuid::new_v4().to_string();
    let record = harness
        .engine()
        .submit_conversion(
            source.id.clone(),
            key.clone(),
            accepted.clone(),
            CancellationToken::new(),
        )
        .unwrap();
    let duplicate = harness
        .engine()
        .submit_conversion(
            source.id.clone(),
            key.clone(),
            accepted.clone(),
            CancellationToken::new(),
        )
        .unwrap();
    assert_eq!(record.id, duplicate.id);
    let mut edited = accepted.clone();
    edited.file_name = "later_workspace_edit".into();
    assert!(matches!(
        harness.engine().submit_conversion(
            source.id.clone(),
            key,
            edited,
            CancellationToken::new()
        ),
        Err(EngineError::Configuration { .. })
    ));
    let encoded = harness.complete(&record.id);
    assert_eq!(encoded.file_name, "encoded_video.mp4");
    let mut copy = harness.defaults(&source, Operation::VideoConverter);
    copy.video_codec = VideoCodec::Copy;
    copy.audio_codec = AudioCodec::Copy;
    // Explicit equal values are no-ops; these must remain eligible for copying.
    copy.video_size = VideoSize::Hd720;
    copy.sample_rate = Some(48_000);
    copy.channels = Some(2);
    copy.file_name = "copied_video".into();
    let copied = harness.complete(&harness.submit(&source, copy).id);
    assert_eq!(harness.engine().jobs().unwrap().len(), 2);
    assert_eq!(digest(&fixture("video.mp4")), original_hash);
    harness.expect(vec![
        json!({"path": encoded.output.unwrap().locator, "video_codec":"h264", "width":320,"height":180,"frame_rate":30,"audio_codecs":["aac"],"channels":2,"sample_rate":48000,"duration":2.0}),
        json!({"path": copied.output.unwrap().locator, "video_codec":"h264", "width":320,"height":180,"audio_codecs":["aac"],"channels":2,"sample_rate":48000,"duration":2.0,"source":"video.mp4","preserve_video_packets":true}),
    ]);
}

#[test]
fn real_video_downscale_respects_encoder_alignment_and_target_bounds() {
    let harness = Harness::new("video-downscale");
    let source = harness.inspect("video-large.mp4", Operation::VideoConverter);
    let mut config = harness.defaults(&source, Operation::VideoConverter);
    config.video_codec = VideoCodec::H264;
    config.video_size = VideoSize::Sd480;
    let record = harness.complete(&harness.submit(&source, config).id);
    harness.expect(vec![json!({"path":record.output.unwrap().locator,"video_codec":"h264","width":852,"height":480,"display_aspect_ratio":"16/9","audio_codecs":[],"duration":1.0})]);
}

#[test]
fn complete_rational_frame_rate_choices_encode_actual_cadence() {
    let harness = Harness::new("m3-frame-rates");
    let source = harness.inspect("video.mp4", Operation::VideoConverter);
    let mut cases = Vec::new();
    for (selection, rate) in [
        (FrameRate::Fps23976, 24000.0 / 1001.0),
        (FrameRate::Fps24, 24.0),
        (FrameRate::Fps25, 25.0),
        (FrameRate::Fps2997, 30000.0 / 1001.0),
        (FrameRate::Fps30, 30.0),
        (FrameRate::Fps48, 48.0),
        (FrameRate::Fps50, 50.0),
        (FrameRate::Fps5994, 60000.0 / 1001.0),
        (FrameRate::Fps60, 60.0),
        (FrameRate::Custom, 27.0),
    ] {
        let mut config = harness.defaults(&source, Operation::VideoConverter);
        config.video_codec = VideoCodec::H264;
        config.frame_rate = selection;
        if selection == FrameRate::Custom {
            config.custom_frame_rate = Some(velmux_core::Rational {
                numerator: 54,
                denominator: 2,
            });
        }
        config.file_name = format!("rate_{selection:?}");
        let completed = harness.complete(&harness.submit(&source, config).id);
        cases.push(json!({"path":completed.output.unwrap().locator,"video_codec":"h264","width":320,"height":180,
            "frame_rate":rate,"audio_codecs":["aac"],"channels":2,"sample_rate":48000,"duration":2.0,"tones":true}));
    }
    harness.expect(cases);
}

#[test]
fn portrait_bounds_preserve_aspect_and_only_custom_explicitly_upscales() {
    let harness = Harness::new("m3-custom-bounds");
    let source = harness.inspect("portrait.mp4", Operation::VideoConverter);
    let original_hash = digest(&fixture("portrait.mp4"));
    let mut cases = Vec::new();
    for (size, width, height, bounds) in [
        (VideoSize::P2160, 180, 320, None),
        (VideoSize::Custom, 360, 640, Some((360, 640))),
        (VideoSize::Custom, 168, 300, Some((320, 300))),
    ] {
        let mut config = harness.defaults(&source, Operation::VideoConverter);
        config.video_codec = VideoCodec::H264;
        config.video_size = size;
        if let Some((w, h)) = bounds {
            config.custom_width = Some(w);
            config.custom_height = Some(h);
        }
        config.file_name = format!("portrait_{width}_{height}");
        let completed = harness.complete(&harness.submit(&source, config).id);
        cases.push(json!({"path":completed.output.unwrap().locator,"video_codec":"h264","width":width,"height":height,
            "display_aspect_ratio":"9/16","audio_codecs":["aac"],"channels":2,"sample_rate":48000,"duration":2.0,
            "source":"portrait.mp4","match_source_frame":true,"preserve_video_timestamps":true,"tones":true}));
    }
    let mut bad = harness.defaults(&source, Operation::VideoConverter);
    bad.video_size = VideoSize::Custom;
    bad.custom_width = Some(u32::MAX);
    bad.custom_height = Some(720);
    assert!(matches!(
        harness
            .engine()
            .validate_configuration(source.id.clone(), bad),
        Err(EngineError::Configuration { .. })
    ));
    for (numerator, denominator) in [(30, 0), (0, 1), (241, 1)] {
        let mut bad = harness.defaults(&source, Operation::VideoConverter);
        bad.frame_rate = FrameRate::Custom;
        bad.custom_frame_rate = Some(velmux_core::Rational {
            numerator,
            denominator,
        });
        assert!(matches!(
            harness
                .engine()
                .validate_configuration(source.id.clone(), bad),
            Err(EngineError::Configuration { .. })
        ));
    }
    assert_eq!(digest(&fixture("portrait.mp4")), original_hash);
    harness.expect(cases);
}

#[test]
fn accepted_m2_request_without_new_fields_restarts_without_replanning_or_rewriting() {
    let mut harness = Harness::new("m3-legacy-accepted-request");
    harness
        .provider
        .fail_publications
        .store(1, Ordering::SeqCst);
    let source = harness.inspect("video.mp4", Operation::VideoConverter);
    let mut config = harness.defaults(&source, Operation::VideoConverter);
    config.format = OutputFormat::Mp4;
    config.video_codec = VideoCodec::H264;
    config.frame_rate = FrameRate::Fps30;
    config.quality = 21;
    config.file_name = "legacy_accepted".into();
    let job = harness.submit(&source, config);
    let mut record = harness.finished(&job.id);
    assert_eq!(record.state, JobState::Failed);
    harness.close();
    let database = harness.root.join("operational/operations.sqlite3");
    let connection = Connection::open(&database).unwrap();
    let request: String = connection
        .query_row("SELECT request FROM jobs WHERE id=?1", [&job.id], |row| {
            row.get(0)
        })
        .unwrap();
    let mut legacy: Value = serde_json::from_str(&request).unwrap();
    assert_eq!(legacy["version"], 2);
    assert_eq!(legacy["plan"]["output"]["format"], "Mp4");
    legacy.as_object_mut().unwrap().remove("version");
    legacy["plan"].as_object_mut().unwrap().remove("output");
    legacy["plan"]
        .as_object_mut()
        .unwrap()
        .remove("target_size_bytes");
    let accepted = legacy["plan"]["config"].as_object_mut().unwrap();
    for field in [
        "custom_width",
        "custom_height",
        "custom_frame_rate",
        "audio_precision",
        "channel_layout",
        "compression",
    ] {
        accepted.remove(field);
    }
    for stream in legacy["plan"]["streams"].as_array_mut().unwrap() {
        stream.as_object_mut().unwrap().remove("encoding");
        stream.as_object_mut().unwrap().remove("frame_rate");
    }
    let legacy_bytes = serde_json::to_string(&legacy).unwrap();
    connection
        .execute(
            "UPDATE jobs SET request=?1 WHERE id=?2",
            params![legacy_bytes, job.id],
        )
        .unwrap();
    drop(connection);
    record.state = JobState::Interrupted;
    record.verified = false;
    record.verified_size = None;
    record.verified_hash = None;
    harness.persist_record(&record, "interrupted");
    let mut legacy_record = serde_json::to_value(&record).unwrap();
    legacy_record
        .as_object_mut()
        .unwrap()
        .remove("keep_target_miss");
    Connection::open(&database)
        .unwrap()
        .execute(
            "UPDATE jobs SET record=?1 WHERE id=?2",
            params![serde_json::to_string(&legacy_record).unwrap(), job.id],
        )
        .unwrap();
    harness.reopen();
    harness.engine().restart_job(job.id.clone()).unwrap();
    let completed = harness.complete(&job.id);
    let persisted: String = Connection::open(&database)
        .unwrap()
        .query_row("SELECT request FROM jobs WHERE id=?1", [&job.id], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(
        persisted, legacy_bytes,
        "An immutable legacy accepted request was rewritten"
    );
    assert_eq!(completed.file_name, "legacy_accepted.mp4");
    assert!(!completed.keep_target_miss);
    harness.expect(vec![json!({"path":completed.output.unwrap().locator,"video_codec":"h264","width":320,"height":180,
        "frame_rate":30,"audio_codecs":["aac"],"channels":2,"sample_rate":48000,"duration":2.0,"tones":true})]);
}

#[test]
fn unsupported_accepted_request_version_preserves_operational_records_and_work() {
    let mut harness = Harness::new("m3-future-accepted-request");
    harness
        .provider
        .fail_publications
        .store(1, Ordering::SeqCst);
    let source = harness.inspect("video.mp4", Operation::VideoConverter);
    let original_hash = digest(&fixture("video.mp4"));
    let mut config = harness.defaults(&source, Operation::VideoConverter);
    config.format = OutputFormat::Mp4;
    config.video_codec = VideoCodec::H264;
    config.file_name = "future_accepted".into();
    let job = harness.submit(&source, config);
    let record = harness.finished(&job.id);
    assert_eq!(record.state, JobState::Failed);
    assert!(record.verified);
    harness.close();

    let database = harness.root.join("operational/operations.sqlite3");
    let working = harness
        .root
        .join("operational/jobs")
        .join(&job.id)
        .join("output.mp4");
    let working_hash = digest(&working);
    let connection = Connection::open(&database).unwrap();
    let (request, record_bytes): (String, String) = connection
        .query_row(
            "SELECT request,record FROM jobs WHERE id=?1",
            [&job.id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    let mut future: Value = serde_json::from_str(&request).unwrap();
    future["version"] = json!(99);
    // Refuse the version before trying to interpret a newer enum vocabulary.
    future["plan"]["config"]["format"] = json!("FutureContainer");
    let future_bytes = serde_json::to_string(&future).unwrap();
    connection
        .execute(
            "UPDATE jobs SET request=?1 WHERE id=?2",
            params![future_bytes, job.id],
        )
        .unwrap();
    drop(connection);

    let engine = Engine::new(Box::new(HostNativeProvider(
        harness.root.join("operational"),
    )));
    let error = engine.initialize_media(Box::new(FilesystemProvider(harness.provider.clone())));
    assert!(
        matches!(error, Err(EngineError::Recovery { detail }) if detail.contains("unsupported version 99"))
    );
    drop(engine);

    let connection = Connection::open(&database).unwrap();
    let retained: (String, String) = connection
        .query_row(
            "SELECT request,record FROM jobs WHERE id=?1",
            [&job.id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(retained, (future_bytes, record_bytes));
    let schema_version: i64 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(schema_version, 1);
    assert_eq!(digest(&working), working_hash);
    assert_eq!(digest(&fixture("video.mp4")), original_hash);
}

#[test]
fn real_audio_encoders_resampling_channels_and_copy() {
    let harness = Harness::new("audio");
    let source = harness.inspect("stereo.flac", Operation::AudioConverter);
    let mut cases = Vec::new();
    for (format, codec, expected) in [
        (OutputFormat::M4a, AudioCodec::Aac, "aac"),
        (OutputFormat::Mp3, AudioCodec::Mp3, "mp3"),
        (OutputFormat::Wav, AudioCodec::PcmS16le, "pcm_s16le"),
        (OutputFormat::Flac, AudioCodec::Flac, "flac"),
    ] {
        let mut config = harness.defaults(&source, Operation::AudioConverter);
        config.format = format;
        config.audio_codec = codec;
        config.channels = Some(1);
        config.sample_rate = Some(44_100);
        config.file_name = format!("tone_{}", format.extension());
        let result = harness.complete(&harness.submit(&source, config).id);
        cases.push(expectation(&result, expected, 1, 44_100));
    }
    let mut copy = harness.defaults(&source, Operation::AudioConverter);
    copy.format = OutputFormat::Flac;
    copy.audio_codec = AudioCodec::Copy;
    let copied = harness.complete(&harness.submit(&source, copy).id);
    cases.push(expectation(&copied, "flac", 2, 48_000));
    harness.expect(cases);
}

#[test]
fn original_vfr_and_positive_origin_preserve_real_packet_timing() {
    let harness = Harness::new("timing");
    let mut cases = Vec::new();
    for name in ["vfr.mkv", "positive-origin.mkv"] {
        let source = harness.inspect(name, Operation::VideoConverter);
        let mut config = harness.defaults(&source, Operation::VideoConverter);
        config.format = OutputFormat::Matroska;
        config.video_codec = VideoCodec::Copy;
        config.audio_codec = AudioCodec::Copy;
        let result = harness.complete(&harness.submit(&source, config).id);
        cases.push(json!({"path":result.output.unwrap().locator,"video_codec":"h264","width":320,"height":180,
            "audio_codecs":if name=="vfr.mkv" {vec![]} else {vec!["aac"]},"channels":2,"sample_rate":48000,
            "source":name,"preserve_video_packets":true,"preserve_av_offset":name!="vfr.mkv"}));
        let mut encode = harness.defaults(&source, Operation::VideoConverter);
        encode.format = OutputFormat::Matroska;
        encode.video_codec = VideoCodec::H264;
        if source
            .info
            .streams
            .iter()
            .any(|stream| stream.kind == StreamKind::Audio)
        {
            encode.audio_codec = AudioCodec::Aac;
        }
        encode.file_name = format!(
            "{}_encoded",
            Path::new(name).file_stem().unwrap().to_str().unwrap()
        );
        let encoded = harness.complete(&harness.submit(&source, encode).id);
        cases.push(json!({"path":encoded.output.unwrap().locator,"video_codec":"h264","width":320,"height":180,
            "audio_codecs":if name=="vfr.mkv" {vec![]} else {vec!["aac"]},"channels":2,"sample_rate":48000,
            "source":name,"preserve_video_timestamps":true,"preserve_av_offset":name!="vfr.mkv"}));
    }
    harness.expect(cases);
}

#[test]
fn real_inspection_and_invalid_direct_core_requests() {
    let harness = Harness::new("inspection");
    let source = harness.inspect("stereo.flac", Operation::AudioConverter);
    let options = harness
        .engine()
        .media_catalog_for_source(source.id.clone(), Operation::AudioConverter)
        .unwrap();
    assert!(!options
        .iter()
        .find(|option| option.format == OutputFormat::Mp3)
        .unwrap()
        .audio_codecs
        .contains(&AudioCodec::Copy));
    assert!(options
        .iter()
        .find(|option| option.format == OutputFormat::Flac)
        .unwrap()
        .audio_codecs
        .contains(&AudioCodec::Copy));
    for name in [
        " reserved ",
        "bad*name",
        "bad?name",
        "bad\"name",
        "bad|name",
    ] {
        let mut config = harness.defaults(&source, Operation::AudioConverter);
        config.file_name = name.into();
        assert!(matches!(
            harness
                .engine()
                .validate_configuration(source.id.clone(), config),
            Err(EngineError::Configuration { .. })
        ));
    }
    let rotated = harness.inspect("rotated-sar.mp4", Operation::VideoConverter);
    let video = rotated
        .info
        .streams
        .iter()
        .find_map(|stream| stream.video.as_ref())
        .unwrap();
    let sar = video.sample_aspect_ratio.as_ref().unwrap();
    assert_eq!((sar.numerator, sar.denominator), (4, 3));
    assert_eq!(
        video
            .rotation_degrees_counter_clockwise
            .unwrap_or_else(|| panic!("Missing rotation; inspected video facts: {video:?}"))
            .abs()
            .round(),
        90.0
    );
    let tracks = harness.inspect("multi-track.mkv", Operation::VideoConverter);
    let tracks_default = harness
        .engine()
        .default_configuration(tracks.id.clone(), Operation::VideoConverter)
        .unwrap();
    assert_eq!(tracks_default.format, OutputFormat::Original);
    harness
        .engine()
        .validate_configuration(tracks.id.clone(), tracks_default)
        .unwrap();
    assert_eq!(
        tracks
            .info
            .streams
            .iter()
            .filter(|stream| stream.kind == StreamKind::Audio)
            .count(),
        2
    );
    assert_eq!(
        tracks
            .info
            .streams
            .iter()
            .filter(|stream| stream.kind == StreamKind::Subtitle)
            .count(),
        1
    );
    let languages: Vec<_> = tracks
        .info
        .streams
        .iter()
        .flat_map(|stream| &stream.metadata)
        .filter(|entry| entry.key == "language")
        .map(|entry| entry.value.as_str())
        .collect();
    assert!(languages.contains(&"eng") && languages.contains(&"fra"));
    let hdr = harness.inspect("hdr.mkv", Operation::VideoConverter);
    let video = hdr
        .info
        .streams
        .iter()
        .find_map(|stream| stream.video.as_ref())
        .unwrap();
    assert_eq!(video.bit_depth, Some(10));
    assert_eq!(video.color_transfer.as_deref(), Some("smpte2084"));
    let mut config = harness
        .engine()
        .default_configuration(hdr.id.clone(), Operation::VideoConverter)
        .unwrap();
    assert_eq!(config.format, OutputFormat::Original);
    harness
        .engine()
        .validate_configuration(hdr.id.clone(), config.clone())
        .unwrap();
    config.video_codec = VideoCodec::H264;
    assert!(matches!(
        harness
            .engine()
            .validate_configuration(hdr.id.clone(), config),
        Err(EngineError::Configuration { .. })
    ));
    let mut invalid = harness.defaults(&rotated, Operation::VideoConverter);
    invalid.file_name = "../original".into();
    assert!(matches!(
        harness.engine().submit_conversion(
            rotated.id.clone(),
            uuid::Uuid::new_v4().to_string(),
            invalid,
            CancellationToken::new()
        ),
        Err(EngineError::Configuration { .. })
    ));
    let mut incompatible = harness.defaults(&tracks, Operation::VideoConverter);
    incompatible.format = OutputFormat::Mp4;
    assert!(
        harness
            .engine()
            .validate_configuration(tracks.id.clone(), incompatible)
            .is_err(),
        "SRT cannot silently disappear from MP4 output"
    );
    for name in ["invalid.bin", "truncated.mp4"] {
        assert!(harness
            .engine()
            .inspect_source(
                fixture(name).to_string_lossy().into_owned(),
                name.into(),
                Operation::VideoConverter,
                CancellationToken::new()
            )
            .is_err());
    }
    assert!(harness
        .engine()
        .inspect_source(
            fixture("video.mp4").to_string_lossy().into_owned(),
            "video.mp4".into(),
            Operation::AudioConverter,
            CancellationToken::new()
        )
        .is_err());
    assert!(harness.engine().jobs().unwrap().is_empty());
}

#[test]
fn cancellation_waits_for_native_worker_before_owned_scratch_cleanup() {
    let harness = Harness::new("cancellation");
    harness.provider.block_source.store(true, Ordering::SeqCst);
    let source = harness.inspect("video.mp4", Operation::VideoConverter);
    let job = harness.submit(
        &source,
        harness.defaults(&source, Operation::VideoConverter),
    );
    harness.provider.source_gate.wait_entered();
    let locator = source.locator.clone();
    harness.engine().release_inspection(source.id).unwrap();
    let scratch = harness.root.join("operational/jobs").join(&job.id);
    fs::create_dir_all(&scratch).unwrap();
    fs::write(scratch.join("in_use"), b"worker-owned marker").unwrap();
    harness.engine().cancel_job(job.id.clone()).unwrap();
    assert_eq!(harness.record(&job.id).state, JobState::Canceling);
    assert!(
        scratch.join("in_use").exists(),
        "Cancellation deleted resources before worker acknowledgement"
    );
    assert!(
        harness
            .provider
            .retained_sources
            .lock()
            .unwrap()
            .contains(&locator),
        "Cancellation revoked source access before the worker acknowledged termination"
    );
    harness.provider.source_gate.release();
    assert_eq!(harness.finished(&job.id).state, JobState::Canceled);
    let deadline = Instant::now() + Duration::from_secs(2);
    while (scratch.exists()
        || harness
            .provider
            .retained_sources
            .lock()
            .unwrap()
            .contains(&locator))
        && Instant::now() < deadline
    {
        thread::sleep(Duration::from_millis(5));
    }
    assert!(!scratch.exists());
    assert!(!harness
        .provider
        .retained_sources
        .lock()
        .unwrap()
        .contains(&locator));
    assert_eq!(harness.provider.worker_sources.load(Ordering::SeqCst), 0);
    assert!(harness.provider.leases.lock().unwrap().is_empty());
    assert_eq!(harness.provider.publish_calls.load(Ordering::SeqCst), 0);
}

#[test]
fn one_worker_queues_second_job_and_late_cancel_preserves_published_result() {
    let harness = Harness::new("bounded-worker");
    harness
        .provider
        .block_publication
        .store(true, Ordering::SeqCst);
    let source = harness.inspect("video.mp4", Operation::VideoConverter);
    let first = harness.submit(
        &source,
        harness.defaults(&source, Operation::VideoConverter),
    );
    harness.provider.publication_gate.wait_entered();
    let second = harness.submit(
        &source,
        harness.defaults(&source, Operation::VideoConverter),
    );
    assert_eq!(harness.record(&second.id).state, JobState::Queued);
    harness.engine().cancel_job(first.id.clone()).unwrap();
    assert_eq!(harness.record(&first.id).state, JobState::Canceling);
    harness.provider.publication_gate.release();
    let result = harness.complete(&first.id);
    harness.complete(&second.id);
    assert!(Path::new(&result.output.unwrap().locator).is_file());
    assert_eq!(
        harness
            .provider
            .maximum_worker_sources
            .load(Ordering::SeqCst),
        1
    );
}

#[test]
fn verified_work_is_reused_after_publication_failure_without_reencoding() {
    let harness = Harness::new("publication-retry");
    harness
        .provider
        .fail_publications
        .store(1, Ordering::SeqCst);
    let source = harness.inspect("stereo.flac", Operation::AudioConverter);
    let job = harness.submit(
        &source,
        harness.defaults(&source, Operation::AudioConverter),
    );
    let failed = harness.finished(&job.id);
    assert_eq!(failed.state, JobState::Failed);
    assert!(failed.verified);
    let retained = harness.work(&failed);
    let hash = digest(&retained);
    let opens = harness.provider.open_calls.load(Ordering::SeqCst);
    harness.engine().retry_publication(job.id.clone()).unwrap();
    let completed = harness.complete(&job.id);
    assert_eq!(
        digest(Path::new(&completed.output.as_ref().unwrap().locator)),
        hash
    );
    assert_eq!(
        harness.provider.open_calls.load(Ordering::SeqCst),
        opens,
        "Saving a valid working result reopened/reencoded the source"
    );
    harness.expect(vec![expectation(&completed, "aac", 2, 48_000)]);
}

#[test]
fn changed_retained_work_is_never_published_as_verified() {
    let harness = Harness::new("publication-corruption");
    harness
        .provider
        .fail_publications
        .store(1, Ordering::SeqCst);
    let source = harness.inspect("stereo.flac", Operation::AudioConverter);
    let job = harness.submit(
        &source,
        harness.defaults(&source, Operation::AudioConverter),
    );
    let failed = harness.finished(&job.id);
    assert_eq!(failed.state, JobState::Failed);
    let path = harness.work(&failed);
    let mut bytes = fs::read(&path).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 1;
    fs::write(&path, bytes).unwrap();
    harness.engine().retry_publication(job.id.clone()).unwrap();
    let retried = harness.finished(&job.id);
    assert_eq!(retried.state, JobState::Failed);
    assert_eq!(retried.error_category.as_deref(), Some("recovery"));
    assert_eq!(harness.provider.publish_calls.load(Ordering::SeqCst), 1);
    assert!(retried.output.is_none());
}

#[test]
fn real_publication_survives_response_loss_and_sqlite_reconciliation() {
    let mut harness = Harness::new("publication-reconcile");
    harness
        .provider
        .fail_after_publish
        .store(true, Ordering::SeqCst);
    let source = harness.inspect("video.mp4", Operation::VideoConverter);
    let job = harness.submit(
        &source,
        harness.defaults(&source, Operation::VideoConverter),
    );
    let completed = harness.complete(&job.id);
    let path = PathBuf::from(&completed.output.as_ref().unwrap().locator);
    let hash = digest(&path);
    harness.close();
    let mut interrupted = completed.clone();
    interrupted.state = JobState::Failed;
    interrupted.output = None;
    harness.persist_record(&interrupted, "failed");
    harness.reopen();
    let restored = harness.complete(&job.id);
    assert_eq!(restored.output.unwrap().locator, path.to_string_lossy());
    assert_eq!(digest(&path), hash);
    assert_eq!(harness.provider.publish_calls.load(Ordering::SeqCst), 1);
    harness.engine().dismiss_job(job.id).unwrap();
    assert_eq!(
        digest(&path),
        hash,
        "Dismissing a job removed its published file"
    );
}

#[test]
fn sqlite_restore_distinguishes_interruption_manual_pause_and_cancellation() {
    let mut harness = Harness::new("sqlite-restore");
    harness
        .provider
        .fail_publications
        .store(3, Ordering::SeqCst);
    let source = harness.inspect("stereo.flac", Operation::AudioConverter);
    let mut records = Vec::new();
    for _ in 0..3 {
        let job = harness.submit(
            &source,
            harness.defaults(&source, Operation::AudioConverter),
        );
        let failed = harness.finished(&job.id);
        assert_eq!(failed.state, JobState::Failed);
        records.push(failed);
    }
    harness.close();
    for (index, state, label) in [
        (0, JobState::Processing, "processing"),
        (1, JobState::Paused, "paused"),
        (2, JobState::Canceling, "canceling"),
    ] {
        let record = &mut records[index];
        record.state = state;
        record.verified = false;
        record.verified_size = None;
        record.verified_hash = None;
        harness.persist_record(record, label);
    }
    harness.reopen();
    assert_eq!(harness.record(&records[0].id).state, JobState::Interrupted);
    assert_eq!(harness.record(&records[1].id).state, JobState::Paused);
    assert_eq!(harness.record(&records[2].id).state, JobState::Canceled);
    assert!(harness.work(&records[0]).exists());
    assert!(harness.work(&records[1]).exists());
    assert!(!harness.work(&records[2]).exists());
}

#[test]
fn real_process_kill_restores_interrupted_and_queued_jobs() {
    const TEST_NAME: &str = "real_process_kill_restores_interrupted_and_queued_jobs";
    const CHILD_ENV: &str = "VELMUX_DURABILITY_CHILD";
    const ROOT_NAME: &str = "process-kill-durability";

    if std::env::var_os(CHILD_ENV).as_deref() == Some(std::ffi::OsStr::new("1")) {
        let harness = Harness::new(ROOT_NAME);
        let source = harness.inspect("stereo.flac", Operation::AudioConverter);
        let mut retained_config = harness.defaults(&source, Operation::AudioConverter);
        retained_config.format = OutputFormat::Wav;
        retained_config.audio_codec = AudioCodec::PcmS16le;
        retained_config.file_name = "published_before_process_kill".into();
        let retained = harness.complete(&harness.submit(&source, retained_config).id);
        let retained_hash = digest(Path::new(&retained.output.as_ref().unwrap().locator));

        let mut first_config = harness.defaults(&source, Operation::AudioConverter);
        first_config.audio_codec = AudioCodec::Aac;
        first_config.sample_rate = Some(44_100);
        first_config.channels = Some(1);
        first_config.file_name = "accepted_before_process_kill".into();
        let mut second_config = harness.defaults(&source, Operation::AudioConverter);
        second_config.format = OutputFormat::Mp3;
        second_config.audio_codec = AudioCodec::Mp3;
        second_config.file_name = "queued_before_process_kill".into();
        let first_key = uuid::Uuid::new_v4().to_string();
        let second_key = uuid::Uuid::new_v4().to_string();
        harness.provider.block_source.store(true, Ordering::SeqCst);
        let first = harness
            .engine()
            .submit_conversion(
                source.id.clone(),
                first_key.clone(),
                first_config.clone(),
                CancellationToken::new(),
            )
            .unwrap();
        harness.provider.source_gate.wait_entered();
        assert_eq!(harness.record(&first.id).state, JobState::Preparing);
        let second = harness
            .engine()
            .submit_conversion(
                source.id,
                second_key.clone(),
                second_config.clone(),
                CancellationToken::new(),
            )
            .unwrap();
        assert_eq!(harness.record(&second.id).state, JobState::Queued);
        let phase = json!({
            "first": first, "second": second, "retained": retained,
            "first_key": first_key, "second_key": second_key,
            "first_config": first_config, "second_config": second_config,
            "source_hash": digest(&fixture("stereo.flac")), "retained_hash": retained_hash,
        });
        // Keep accepted configuration independent of subsequent workspace edits.
        first_config.file_name = "unaccepted_later_edit".into();
        second_config.channels = Some(1);
        assert_ne!(
            serde_json::to_value(first_config).unwrap(),
            phase["first_config"]
        );
        assert_ne!(
            serde_json::to_value(second_config).unwrap(),
            phase["second_config"]
        );
        // The parent's phase observation means both requests were durably accepted,
        // later edits occurred, and the worker holds an open source FD in its callback.
        let temporary = harness.root.join("phase.tmp");
        let mut file = File::create(&temporary).unwrap();
        file.write_all(&serde_json::to_vec(&phase).unwrap())
            .unwrap();
        file.sync_all().unwrap();
        fs::rename(temporary, harness.root.join("phase.json")).unwrap();
        loop {
            thread::park_timeout(Duration::from_secs(1));
        }
    }

    struct KillOnDrop(std::process::Child);
    impl Drop for KillOnDrop {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let root = repository().join("build/media-test-output").join(ROOT_NAME);
    if root.exists() {
        fs::remove_dir_all(&root).unwrap();
    }
    let mut child = KillOnDrop(
        std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", TEST_NAME, "--test-threads=1", "--nocapture"])
            .env(CHILD_ENV, "1")
            // Preserve the actual child panic in CI if it fails before the
            // durable phase marker; libtest capture otherwise hides it.
            .stdout(std::process::Stdio::inherit())
            .stderr(std::process::Stdio::inherit())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(15);
    let phase_path = root.join("phase.json");
    while !phase_path.exists() {
        let status = child.0.try_wait().unwrap();
        assert!(
            status.is_none(),
            "Durability child exited before accepting both jobs: {status:?}; see child output"
        );
        assert!(
            Instant::now() < deadline,
            "Durability child did not reach its native Preparing gate"
        );
        thread::sleep(Duration::from_millis(10));
    }
    let phase: Value = serde_json::from_slice(&fs::read(phase_path).unwrap()).unwrap();
    child.0.kill().unwrap();
    let killed = child.0.wait().unwrap();
    use std::os::unix::process::ExitStatusExt;
    assert_eq!(
        killed.signal(),
        Some(9),
        "Expected a real SIGKILL: {killed:?}"
    );
    let first: JobRecord = serde_json::from_value(phase["first"].clone()).unwrap();
    let second: JobRecord = serde_json::from_value(phase["second"].clone()).unwrap();
    let retained: JobRecord = serde_json::from_value(phase["retained"].clone()).unwrap();
    let first_config: ConversionConfig =
        serde_json::from_value(phase["first_config"].clone()).unwrap();
    let second_config: ConversionConfig =
        serde_json::from_value(phase["second_config"].clone()).unwrap();
    let first_key = phase["first_key"].as_str().unwrap();
    let second_key = phase["second_key"].as_str().unwrap();
    let database = root.join("operational/operations.sqlite3");
    let accepted_request = |id: &str| -> (String, Value, String) {
        let (key, request, state): (String, String, String) = Connection::open(&database)
            .unwrap()
            .query_row(
                "SELECT submission_key,request,state FROM jobs WHERE id=?1",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        (key, serde_json::from_str(&request).unwrap(), state)
    };
    let first_saved = accepted_request(&first.id);
    let second_saved = accepted_request(&second.id);
    assert_eq!(first_saved.2, "preparing");
    assert_eq!(second_saved.2, "queued");
    assert_eq!(first_saved.0, first_key);
    assert_eq!(second_saved.0, second_key);
    assert_eq!(first_saved.1["plan"]["config"], phase["first_config"]);
    assert_eq!(second_saved.1["plan"]["config"], phase["second_config"]);

    // Reopen existing SQLite and files, deliberately bypassing Harness::new cleanup.
    let provider = ProviderState::new(root.clone());
    let mut harness = Harness {
        root,
        engine: None,
        provider,
    };
    harness.reopen();
    let interrupted = harness.record(&first.id);
    assert_eq!(interrupted.state, JobState::Interrupted);
    assert_eq!(interrupted.error_category.as_deref(), Some("interrupted"));
    assert!(interrupted.output.is_none());
    let queued_result = harness.complete(&second.id);
    assert_eq!(
        harness.record(&first.id).state,
        JobState::Interrupted,
        "Interrupted work resumed without an explicit restart"
    );
    assert_eq!(harness.provider.publish_calls.load(Ordering::SeqCst), 1);
    assert_eq!(harness.engine().jobs().unwrap().len(), 3);
    assert_eq!(queued_result.file_name, "queued_before_process_kill.mp3");
    let source = harness.inspect("stereo.flac", Operation::AudioConverter);
    for (record, key, config) in [
        (&first, first_key, &first_config),
        (&second, second_key, &second_config),
    ] {
        let duplicate = harness
            .engine()
            .submit_conversion(
                source.id.clone(),
                key.into(),
                config.clone(),
                CancellationToken::new(),
            )
            .unwrap();
        assert_eq!(duplicate.id, record.id);
    }
    let mut edited = first_config.clone();
    edited.file_name = "conflicting_after_process_kill".into();
    assert!(matches!(
        harness.engine().submit_conversion(
            source.id,
            first_key.into(),
            edited,
            CancellationToken::new()
        ),
        Err(EngineError::Configuration { .. })
    ));
    assert_eq!(harness.provider.publish_calls.load(Ordering::SeqCst), 1);
    harness.engine().restart_job(first.id.clone()).unwrap();
    let restarted = harness.complete(&first.id);
    assert_eq!(restarted.file_name, "accepted_before_process_kill.m4a");
    assert_eq!(harness.provider.publish_calls.load(Ordering::SeqCst), 2);
    assert_eq!(accepted_request(&first.id).1, first_saved.1);
    assert_eq!(accepted_request(&second.id).1, second_saved.1);
    assert_eq!(
        digest(&fixture("stereo.flac")),
        phase["source_hash"].as_str().unwrap()
    );
    let retained_path = Path::new(&retained.output.as_ref().unwrap().locator);
    assert_eq!(
        digest(retained_path),
        phase["retained_hash"].as_str().unwrap()
    );
    harness.engine().dismiss_job(retained.id.clone()).unwrap();
    harness.engine().clear_cache().unwrap();
    harness.close();
    harness.reopen();
    assert_eq!(harness.record(&first.id).state, JobState::Completed);
    assert_eq!(harness.record(&second.id).state, JobState::Completed);
    assert_eq!(harness.provider.publish_calls.load(Ordering::SeqCst), 2);
    assert_eq!(
        digest(retained_path),
        phase["retained_hash"].as_str().unwrap()
    );
    harness.expect(vec![
        expectation(&retained, "pcm_s16le", 2, 48_000),
        expectation(&queued_result, "mp3", 2, 48_000),
        expectation(&restarted, "aac", 1, 44_100),
    ]);
}

#[test]
fn durable_source_access_follows_workspace_and_unfinished_job_ownership() {
    let mut harness = Harness::new("source-access-lifetime");
    let locator = fixture("stereo.flac").to_string_lossy().into_owned();
    let original_hash = digest(Path::new(&locator));
    let first = harness.inspect("stereo.flac", Operation::AudioConverter);
    let second = harness.inspect("stereo.flac", Operation::AudioConverter);
    let retains = || {
        harness
            .provider
            .access_events
            .lock()
            .unwrap()
            .iter()
            .filter(|(event, value)| event == "retain" && value == &locator)
            .count()
    };
    assert_eq!(
        retains(),
        1,
        "Two workspaces must share one durable native acquisition"
    );
    harness.engine().release_inspection(first.id).unwrap();
    assert!(harness
        .provider
        .retained_sources
        .lock()
        .unwrap()
        .contains(&locator));
    harness.engine().release_inspection(second.id).unwrap();
    assert!(!harness
        .provider
        .retained_sources
        .lock()
        .unwrap()
        .contains(&locator));

    let canceled = CancellationToken::new();
    canceled.cancel();
    assert!(matches!(
        harness.engine().inspect_source(
            locator.clone(),
            "stereo.flac".into(),
            Operation::AudioConverter,
            canceled
        ),
        Err(EngineError::Cancelled)
    ));
    assert!(!harness
        .provider
        .retained_sources
        .lock()
        .unwrap()
        .contains(&locator));
    assert!(harness.provider.lease_locators.lock().unwrap().is_empty());

    let source = harness.inspect("stereo.flac", Operation::AudioConverter);
    harness
        .provider
        .fail_publications
        .store(1, Ordering::SeqCst);
    let mut config = harness.defaults(&source, Operation::AudioConverter);
    config.audio_codec = AudioCodec::Aac;
    let accepted = harness.submit(&source, config);
    harness.engine().release_inspection(source.id).unwrap();
    let failed = harness.finished(&accepted.id);
    assert_eq!(failed.state, JobState::Failed);
    assert!(harness
        .provider
        .retained_sources
        .lock()
        .unwrap()
        .contains(&locator));
    harness.close();
    assert!(
        harness
            .provider
            .retained_sources
            .lock()
            .unwrap()
            .contains(&locator),
        "Runtime shutdown revoked a durable unfinished job"
    );
    let orphan = fixture("video.mp4").to_string_lossy().into_owned();
    harness
        .provider
        .retained_sources
        .lock()
        .unwrap()
        .insert(orphan.clone());
    harness.reopen();
    assert!(harness
        .provider
        .retained_sources
        .lock()
        .unwrap()
        .contains(&locator));
    assert!(
        !harness
            .provider
            .retained_sources
            .lock()
            .unwrap()
            .contains(&orphan),
        "Startup did not reconcile an owned orphan access receipt"
    );
    harness.engine().clear_cache().unwrap();
    assert!(harness
        .provider
        .retained_sources
        .lock()
        .unwrap()
        .contains(&locator));
    harness.engine().restart_job(accepted.id.clone()).unwrap();
    let completed = harness.complete(&accepted.id);
    let deadline = Instant::now() + Duration::from_secs(5);
    while harness
        .provider
        .retained_sources
        .lock()
        .unwrap()
        .contains(&locator)
    {
        assert!(
            Instant::now() < deadline,
            "Completed work retained unused native document access"
        );
        thread::sleep(Duration::from_millis(10));
    }
    assert!(harness.provider.lease_locators.lock().unwrap().is_empty());
    assert_eq!(digest(Path::new(&locator)), original_hash);
    let published = PathBuf::from(&completed.output.as_ref().unwrap().locator);
    let output_hash = digest(&published);
    harness.engine().dismiss_job(accepted.id).unwrap();
    harness.close();
    harness.reopen();
    assert!(!harness
        .provider
        .retained_sources
        .lock()
        .unwrap()
        .contains(&locator));
    assert_eq!(digest(&published), output_hash);
    harness.expect(vec![expectation(&completed, "aac", 2, 48_000)]);
}

#[test]
fn concurrent_source_access_release_protects_inflight_opens_and_other_locators() {
    let harness = Harness::new("source-access-concurrent");
    let source = harness.inspect("video.mp4", Operation::VideoConverter);
    let locator = fixture("video.mp4").to_string_lossy().into_owned();
    harness.provider.block_preview.store(true, Ordering::SeqCst);
    let engine = harness.engine().clone();
    let id = source.id.clone();
    let preview = thread::Builder::new()
        .name("velmux-preview-test".into())
        .spawn(move || engine.prepare_preview(id, 0, CancellationToken::new()))
        .unwrap();
    harness.provider.preview_gate.wait_entered();
    harness.engine().release_inspection(source.id).unwrap();
    assert!(
        harness
            .provider
            .retained_sources
            .lock()
            .unwrap()
            .contains(&locator),
        "An in-flight descriptor open lost its source-access reference"
    );
    harness.provider.preview_gate.release();
    assert!(matches!(
        preview.join().unwrap(),
        Err(EngineError::Cancelled)
    ));
    assert!(!harness
        .provider
        .retained_sources
        .lock()
        .unwrap()
        .contains(&locator));

    let source = harness.inspect("video.mp4", Operation::VideoConverter);
    harness
        .provider
        .block_access_release
        .store(true, Ordering::SeqCst);
    let engine = harness.engine().clone();
    let releasing = thread::spawn(move || engine.release_inspection(source.id));
    harness.provider.access_release_gate.wait_entered();
    let engine = harness.engine().clone();
    let (same_sent, same_received) = std::sync::mpsc::channel();
    let same = thread::spawn(move || {
        let result = engine.inspect_source(
            fixture("video.mp4").to_string_lossy().into_owned(),
            "video.mp4".into(),
            Operation::VideoConverter,
            CancellationToken::new(),
        );
        same_sent.send(result).unwrap();
    });
    let waited_for_release = matches!(
        same_received.recv_timeout(Duration::from_millis(50)),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout)
    );
    let engine = harness.engine().clone();
    let (sent, received) = std::sync::mpsc::channel();
    let other = thread::spawn(move || {
        let result = engine.inspect_source(
            fixture("stereo.flac").to_string_lossy().into_owned(),
            "stereo.flac".into(),
            Operation::AudioConverter,
            CancellationToken::new(),
        );
        sent.send(result).unwrap();
    });
    let independent = received.recv_timeout(Duration::from_secs(5));
    // Release before asserting so an actual locking regression cannot orphan
    // threads or leave Engine teardown waiting for our controlled native call.
    harness.provider.access_release_gate.release();
    releasing.join().unwrap().unwrap();
    other.join().unwrap();
    same.join().unwrap();
    assert!(
        waited_for_release,
        "The same locator reopened before its previous native access release returned"
    );
    let same = same_received
        .recv_timeout(Duration::from_secs(1))
        .unwrap()
        .unwrap();
    let independent = independent
        .expect("A slow release blocked unrelated source inspection")
        .unwrap();
    harness
        .provider
        .block_access_release
        .store(false, Ordering::SeqCst);
    harness.engine().release_inspection(independent.id).unwrap();
    harness.engine().release_inspection(same.id).unwrap();
    assert!(harness.provider.retained_sources.lock().unwrap().is_empty());
    assert!(harness.provider.lease_locators.lock().unwrap().is_empty());
}

struct SourceMutation {
    engine: Weak<Engine>,
    path: PathBuf,
    changed: AtomicBool,
}
impl JobObserver for SourceMutation {
    fn on_jobs_changed(&self) {
        if self.changed.load(Ordering::SeqCst) {
            return;
        }
        if let Some(engine) = self.engine.upgrade() {
            if engine
                .jobs()
                .unwrap()
                .iter()
                .any(|job| job.state == JobState::Processing)
                && !self.changed.swap(true, Ordering::SeqCst)
            {
                // Append without invalidating the original container. The reader's
                // accepted range stays valid, but the source identity has changed.
                OpenOptions::new()
                    .append(true)
                    .open(&self.path)
                    .unwrap()
                    .write_all(b"changed during processing")
                    .unwrap();
            }
        }
    }
}

#[test]
fn source_change_after_worker_validation_prevents_publication() {
    let harness = Harness::new("changed-source");
    let path = harness.root.join("editable-source.mp4");
    fs::copy(fixture("video.mp4"), &path).unwrap();
    let source = harness
        .engine()
        .inspect_source(
            path.to_string_lossy().into_owned(),
            "editable-source.mp4".into(),
            Operation::VideoConverter,
            CancellationToken::new(),
        )
        .unwrap();
    harness
        .engine()
        .observe_jobs(Box::new(SourceMutation {
            engine: Arc::downgrade(harness.engine()),
            path,
            changed: AtomicBool::new(false),
        }))
        .unwrap();
    let job = harness.submit(
        &source,
        harness.defaults(&source, Operation::VideoConverter),
    );
    let record = harness.finished(&job.id);
    assert_eq!(record.state, JobState::Failed);
    assert_eq!(record.error_category.as_deref(), Some("source_access"));
    assert_eq!(harness.provider.publish_calls.load(Ordering::SeqCst), 0);
}

#[test]
fn store_ownership_and_worker_persistence_failures_are_observable() {
    let harness = Harness::new("store-health");
    let contender = Engine::new(Box::new(HostNativeProvider(
        harness.root.join("operational"),
    )));
    assert!(contender
        .initialize_media(Box::new(FilesystemProvider(harness.provider.clone())))
        .is_err());
    let source = harness.inspect("stereo.flac", Operation::AudioConverter);
    let connection = Connection::open(harness.root.join("operational/operations.sqlite3")).unwrap();
    connection.execute_batch("CREATE TRIGGER controlled_claim_failure BEFORE UPDATE ON jobs WHEN NEW.state='preparing' BEGIN SELECT RAISE(FAIL,'controlled claim failure'); END;").unwrap();
    let job = harness.submit(
        &source,
        harness.defaults(&source, Operation::AudioConverter),
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    while harness.engine().runtime_error().unwrap().is_none() {
        assert!(
            Instant::now() < deadline,
            "A scheduler storage failure was hidden"
        );
        thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(harness.record(&job.id).state, JobState::Queued);
    assert!(harness
        .engine()
        .submit_conversion(
            source.id.clone(),
            uuid::Uuid::new_v4().to_string(),
            harness.defaults(&source, Operation::AudioConverter),
            CancellationToken::new()
        )
        .is_err());
}

#[test]
fn moved_published_file_does_not_block_other_jobs_or_delete_user_content() {
    let mut harness = Harness::new("moved-publication");
    let source = harness.inspect("stereo.flac", Operation::AudioConverter);
    let job = harness.submit(
        &source,
        harness.defaults(&source, Operation::AudioConverter),
    );
    let completed = harness.complete(&job.id);
    let original = PathBuf::from(&completed.output.as_ref().unwrap().locator);
    let moved = harness.root.join("user-moved-result.m4a");
    fs::rename(&original, &moved).unwrap();
    let hash = digest(&moved);
    harness.close();
    let mut interrupted = completed;
    interrupted.state = JobState::Publishing;
    interrupted.output = None;
    harness.persist_record(&interrupted, "publishing");
    harness.reopen();
    assert_eq!(harness.record(&job.id).state, JobState::Failed);
    assert!(harness.engine().runtime_error().unwrap().is_none());
    assert!(!harness
        .inspect("stereo.wav", Operation::AudioConverter)
        .info
        .streams
        .is_empty());
    assert_eq!(digest(&moved), hash);
}

fn cached_preview_path(locator: &str, harness: &Harness) -> PathBuf {
    let path = PathBuf::from(
        locator
            .strip_prefix("file://")
            .expect("Preview must use an actual native file locator"),
    );
    assert!(
        path.starts_with(harness.root.join("cache/previews")),
        "A preview escaped its owned cache"
    );
    assert!(path.is_file());
    path
}

#[test]
fn actual_previews_seek_bound_duration_and_preserve_cache_ownership() {
    let harness = Harness::new("previews");
    let mut cases = Vec::new();
    for (name, operation, start_us, duration, video_size, audio_codec) in [
        (
            "video.mp4",
            Operation::VideoConverter,
            500_000,
            1.5,
            Some((320, 180)),
            Some("aac"),
        ),
        (
            "hdr.mkv",
            Operation::VideoConverter,
            0,
            1.0,
            Some((160, 90)),
            None,
        ),
        (
            "stereo.flac",
            Operation::AudioConverter,
            500_000,
            1.5,
            None,
            Some("pcm_s16le"),
        ),
        (
            "positive-origin.mkv",
            Operation::VideoConverter,
            500_000,
            1.75,
            Some((320, 180)),
            Some("aac"),
        ),
        (
            "long-preview.mp4",
            Operation::VideoConverter,
            20_000_000,
            15.0,
            Some((160, 90)),
            Some("aac"),
        ),
    ] {
        let before = digest(&fixture(name));
        let source = harness.inspect(name, operation);
        let preview = harness
            .engine()
            .prepare_preview(source.id.clone(), start_us, CancellationToken::new())
            .unwrap();
        assert_eq!(preview.start_us, start_us);
        assert!((1..=15_000_000).contains(&preview.duration_us));
        let path = cached_preview_path(&preview.locator, &harness);
        let cache_size = fs::metadata(&path).unwrap().len();
        let preview_hash = digest(&path);
        assert_eq!(harness.engine().cache_bytes().unwrap(), cache_size);
        harness.engine().clear_cache().unwrap();
        assert_eq!(
            digest(&path),
            preview_hash,
            "Clear Cache removed a preview still referenced by a workspace"
        );

        // Preserve exact prepared bytes for independent post-test inspection;
        // production cache ownership still ends with the workspace below.
        let evidence = harness.root.join(format!(
            "{}_preview.{}",
            Path::new(name).file_stem().unwrap().to_str().unwrap(),
            path.extension().unwrap().to_str().unwrap()
        ));
        fs::copy(&path, &evidence).unwrap();
        assert_eq!(digest(&evidence), preview_hash);
        let mut expected = json!({
            "path":evidence,"source":name,"duration":duration,"duration_tolerance":0.15,
            "reported_preview_duration_us":preview.duration_us,"audio_codecs":audio_codec.into_iter().collect::<Vec<_>>(),
            "sample_rate":48000,"channels":2,"tones":audio_codec.is_some(),
        });
        if let Some((width, height)) = video_size {
            expected["video_codec"] = json!("h264");
            expected["width"] = json!(width);
            expected["height"] = json!(height);
            expected["frame_rate"] = json!(30);
            if name == "hdr.mkv" {
                expected["sdr_tone_map"] = json!(true);
            } else {
                expected["preview_source_seek"] = json!(start_us as f64 / 1_000_000.0);
            }
        }
        if name == "long-preview.mp4" {
            expected["duration_range"] = json!([14.85, 15.15]);
            expected["tone_frequencies"] = json!([880, 990]);
        }
        cases.push(expected);
        harness.engine().release_inspection(source.id).unwrap();
        assert!(
            !path.exists(),
            "Closing a workspace retained its obsolete prepared preview"
        );
        assert_eq!(harness.engine().cache_bytes().unwrap(), 0);
        assert_eq!(
            digest(&fixture(name)),
            before,
            "Preview preparation modified the original source"
        );
    }
    assert!(
        harness.engine().jobs().unwrap().is_empty(),
        "Preview preparation created persistent processing jobs"
    );
    assert_eq!(harness.provider.publish_calls.load(Ordering::SeqCst), 0);
    harness.expect(cases);
}

#[test]
fn canceled_and_obsolete_preview_requests_release_native_sources_and_scratch() {
    let harness = Harness::new("preview-cancellation");
    let source = harness.inspect("video.mp4", Operation::VideoConverter);
    let canceled = CancellationToken::new();
    canceled.cancel();
    assert!(matches!(
        harness
            .engine()
            .prepare_preview(source.id.clone(), 0, canceled),
        Err(EngineError::Cancelled)
    ));
    assert_eq!(harness.engine().cache_bytes().unwrap(), 0);
    assert!(harness.provider.leases.lock().unwrap().is_empty());

    harness.provider.block_preview.store(true, Ordering::SeqCst);
    let engine = harness.engine().clone();
    let source_id = source.id.clone();
    let obsolete = CancellationToken::new();
    let worker_token = obsolete.clone();
    let preparation = thread::Builder::new()
        .name("velmux-preview-test".into())
        .spawn(move || engine.prepare_preview(source_id, 0, worker_token))
        .unwrap();
    harness.provider.preview_gate.wait_entered();
    let replacement_token = CancellationToken::new();
    let replacement_engine = harness.engine().clone();
    let replacement_source = source.id.clone();
    let replacement_worker_token = replacement_token.clone();
    let replacement_worker = thread::spawn(move || {
        replacement_engine.prepare_preview(replacement_source, 500_000, replacement_worker_token)
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    while !obsolete.is_cancelled() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(5));
    }
    assert!(
        obsolete.is_cancelled(),
        "A superseded preview continued without cancellation"
    );
    assert!(
        !replacement_worker.is_finished(),
        "The replacement bypassed bounded preview ownership while native access was in use"
    );
    // Native access must return before its worker can acknowledge cancellation.
    harness.provider.preview_gate.release();
    assert!(matches!(
        preparation.join().unwrap(),
        Err(EngineError::Cancelled)
    ));
    let replacement = replacement_worker.join().unwrap().unwrap();
    let replacement_path = cached_preview_path(&replacement.locator, &harness);
    let orphan = replacement_path
        .parent()
        .unwrap()
        .join(format!("{}.mp4", uuid::Uuid::new_v4()));
    fs::write(&orphan, b"obsolete disposable cache bytes").unwrap();
    let unidentified = replacement_path.parent().unwrap().join("unidentified-file");
    fs::write(&unidentified, b"preserve unidentified data").unwrap();
    harness.engine().clear_cache().unwrap();
    assert!(!orphan.exists());
    assert!(unidentified.exists());
    assert!(replacement_path.exists());
    harness.engine().release_inspection(source.id).unwrap();
    assert!(replacement_token.is_cancelled());
    assert!(!replacement_path.exists());
    assert!(harness.provider.leases.lock().unwrap().is_empty());
    let remaining: Vec<_> = fs::read_dir(replacement_path.parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(remaining, vec![unidentified]);
    assert!(harness.engine().jobs().unwrap().is_empty());
}

#[test]
fn unmasked_stereo_pcm_encodes_aac_without_losing_channel_identity() {
    let harness = Harness::new("wav-aac");
    let source = harness.inspect("stereo.wav", Operation::AudioConverter);
    let mut config = harness.defaults(&source, Operation::AudioConverter);
    config.audio_codec = AudioCodec::Aac;
    let result = harness.complete(&harness.submit(&source, config).id);
    harness.expect(vec![expectation(&result, "aac", 2, 48_000)]);
}

#[test]
fn configuration_choices_follow_real_sampling_and_encoding_limits() {
    let harness = Harness::new("applicable-settings");
    let pcm32 = harness.inspect("pcm32.wav", Operation::AudioConverter);
    assert!(harness
        .engine()
        .media_catalog_for_source(pcm32.id.clone(), Operation::AudioConverter)
        .unwrap()
        .iter()
        .any(|format| format.format == OutputFormat::Flac),
        "Explicit precision conversion makes FLAC applicable; Original precision must still reject truncation");
    let mut invalid_precision = harness.defaults(&pcm32, Operation::AudioConverter);
    invalid_precision.format = OutputFormat::Flac;
    assert!(matches!(
        harness
            .engine()
            .validate_configuration(pcm32.id, invalid_precision),
        Err(EngineError::Configuration { .. })
    ));

    let odd = harness.inspect("odd-video.mkv", Operation::VideoConverter);
    let odd_default = harness
        .engine()
        .default_configuration(odd.id.clone(), Operation::VideoConverter)
        .unwrap();
    assert_eq!(odd_default.format, OutputFormat::Original);
    assert_eq!(odd_default.video_codec, VideoCodec::Automatic);
    let odd_choices = harness
        .engine()
        .configuration_choices(odd.id.clone(), odd_default.clone())
        .unwrap();
    assert!(odd_choices.frame_rates.contains(&FrameRate::Original));
    let copied = harness.complete(&harness.submit(&odd, odd_default).id);

    let stereo = harness.inspect("stereo.flac", Operation::AudioConverter);
    let mut equal = harness.defaults(&stereo, Operation::AudioConverter);
    equal.format = OutputFormat::Flac;
    equal.audio_codec = AudioCodec::Copy;
    equal.sample_rate = Some(48000);
    equal.channels = Some(2);
    let equal_choices = harness
        .engine()
        .configuration_choices(stereo.id.clone(), equal.clone())
        .unwrap();
    assert!(equal_choices.sample_rates.contains(&48000) && equal_choices.channels.contains(&2));
    harness
        .engine()
        .validate_configuration(stereo.id, equal)
        .unwrap();
    let high = harness.inspect("high-rate.flac", Operation::AudioConverter);
    let mut config = harness
        .engine()
        .default_configuration(high.id.clone(), Operation::AudioConverter)
        .unwrap();
    assert_eq!(
        config.format,
        OutputFormat::Original,
        "Default should retain the source representation instead of assuming AAC supports 192 kHz"
    );
    config.format = OutputFormat::M4a;
    let options = harness
        .engine()
        .configuration_choices(high.id.clone(), config.clone())
        .unwrap();
    assert!(!options.sample_rates.contains(&0));
    assert!(options.sample_rates.contains(&44100) && options.sample_rates.contains(&48000));
    assert!(
        options.sample_rates.iter().all(|rate| [
            8000, 11025, 12000, 16000, 22050, 24000, 32000, 44100, 48000, 64000, 88200, 96000
        ]
        .contains(rate)),
        "Every offered AAC rate must be a declared usable rate"
    );
    assert!(matches!(
        harness
            .engine()
            .validate_configuration(high.id.clone(), config.clone()),
        Err(EngineError::Configuration { .. })
    ));
    config.sample_rate = Some(48000);
    let aac = harness.complete(&harness.submit(&high, config).id);

    let surround = harness.inspect("surround.flac", Operation::AudioConverter);
    let mut mp3 = harness.defaults(&surround, Operation::AudioConverter);
    mp3.format = OutputFormat::Mp3;
    let options = harness
        .engine()
        .configuration_choices(surround.id.clone(), mp3.clone())
        .unwrap();
    assert_eq!(options.channels, vec![1, 2]);
    assert!(matches!(
        harness
            .engine()
            .validate_configuration(surround.id.clone(), mp3.clone()),
        Err(EngineError::Configuration { .. })
    ));
    mp3.channels = Some(2);
    harness
        .engine()
        .validate_configuration(surround.id, mp3)
        .unwrap();

    let low = harness.inspect("low-rate.flac", Operation::AudioConverter);
    let mut config = harness.defaults(&low, Operation::AudioConverter);
    config.format = OutputFormat::Mp3;
    let options = harness
        .engine()
        .configuration_choices(low.id.clone(), config.clone())
        .unwrap();
    assert!(options.audio_bitrates.contains(&0));
    assert!(
        options.audio_bitrates.contains(&64000),
        "Expose applicable explicit MPEG-2.5 bitrates"
    );
    assert!(
        options
            .audio_bitrates
            .iter()
            .all(|rate| [0, 8000, 16000, 24000, 32000, 40000, 48000, 56000, 64000].contains(rate)),
        "Never offer a bitrate beyond the actual 8 kHz MP3 route"
    );
    config.audio_bitrate = Some(192000);
    assert!(matches!(
        harness
            .engine()
            .validate_configuration(low.id.clone(), config.clone()),
        Err(EngineError::Configuration { .. })
    ));
    config.audio_bitrate = None;
    let mp3 = harness.complete(&harness.submit(&low, config).id);
    let mut low_expected = expectation(&mp3, "mp3", 1, 8000);
    low_expected["tone_frequencies"] = json!([440]);
    low_expected["gapless_duration"] = json!(true);
    low_expected["decoded_samples"] = json!(16000);
    harness.expect(vec![expectation(&aac, "aac", 2, 48000), low_expected,
        json!({"path":copied.output.unwrap().locator,"video_codec":"ffv1","width":321,"height":181,"audio_codecs":[],"duration":1.0})]);
}

#[test]
fn exposed_quality_and_bitrate_controls_change_real_encoded_media() {
    let harness = Harness::new("encoding-controls");
    let video = harness.inspect("video.mp4", Operation::VideoConverter);
    let mut config = harness.defaults(&video, Operation::VideoConverter);
    config.video_codec = VideoCodec::H264;
    config.quality = 28;
    config.file_name = "quality_28".into();
    let lower = harness.complete(&harness.submit(&video, config.clone()).id);
    config.quality = 18;
    config.file_name = "quality_18".into();
    let higher = harness.complete(&harness.submit(&video, config).id);
    let video_case = |record: &JobRecord| {
        json!({"path":record.output.as_ref().unwrap().locator,
        "video_codec":"h264","width":320,"height":180,"audio_codecs":["aac"],"sample_rate":48000,
        "channels":2,"tones":true,"duration":2.0})
    };
    let mut higher_case = video_case(&higher);
    higher_case["quality_reference"] = json!(fixture("video.mp4"));
    higher_case["lower_quality_output"] = json!(lower.output.as_ref().unwrap().locator);

    let audio = harness.inspect("stereo.wav", Operation::AudioConverter);
    let mut config = harness.defaults(&audio, Operation::AudioConverter);
    config.audio_codec = AudioCodec::Aac;
    config.audio_bitrate = Some(128000);
    config.file_name = "bitrate_128".into();
    let lower_bitrate = harness.complete(&harness.submit(&audio, config.clone()).id);
    config.audio_bitrate = Some(256000);
    config.file_name = "bitrate_256".into();
    let higher_bitrate = harness.complete(&harness.submit(&audio, config).id);
    let mut higher_audio_case = expectation(&higher_bitrate, "aac", 2, 48000);
    higher_audio_case["lower_bitrate_output"] =
        json!(lower_bitrate.output.as_ref().unwrap().locator);
    harness.expect(vec![
        video_case(&lower),
        higher_case,
        expectation(&lower_bitrate, "aac", 2, 48000),
        higher_audio_case,
    ]);
}

#[test]
fn basic_conversion_preserves_compatible_tracks_metadata_and_chapters() {
    let harness = Harness::new("chaptered-streams");
    let source = harness.inspect("chaptered.mkv", Operation::VideoConverter);
    assert_eq!(source.info.chapters.len(), 2);
    let config = harness
        .engine()
        .default_configuration(source.id.clone(), Operation::VideoConverter)
        .unwrap();
    assert_eq!(config.format, OutputFormat::Original);
    let catalog = harness
        .engine()
        .media_catalog_for_source(source.id.clone(), Operation::VideoConverter)
        .unwrap();
    assert_eq!(
        catalog
            .iter()
            .map(|option| option.format)
            .collect::<Vec<_>>(),
        vec![OutputFormat::Original, OutputFormat::Matroska]
    );
    let mut incompatible = config.clone();
    incompatible.format = OutputFormat::Mp4;
    assert!(matches!(
        harness
            .engine()
            .validate_configuration(source.id.clone(), incompatible),
        Err(EngineError::Configuration { .. })
    ));
    let output = harness.complete(&harness.submit(&source, config).id);
    harness.expect(vec![
        json!({"path":output.output.unwrap().locator,"source":"chaptered.mkv",
        "video_codec":"h264","width":320,"height":180,"audio_codecs":["aac","aac"],
        "sample_rate":48000,"channels":2,"tones":true,"duration":2.0,
        "preserve_video_packets":true,"preserve_auxiliary_tracks_and_chapters":true}),
    ]);
}

#[test]
fn attached_artwork_is_preserved_and_incompatible_catalog_formats_are_omitted() {
    let harness = Harness::new("attached-artwork");
    let original_hash = digest(&fixture("cover-art.flac"));
    let source = harness.inspect("cover-art.flac", Operation::AudioConverter);
    let artwork = source
        .info
        .streams
        .iter()
        .find(|stream| {
            stream
                .video
                .as_ref()
                .is_some_and(|video| video.attached_picture)
        })
        .expect("The real FLAC fixture must expose its attached picture");
    assert_eq!(artwork.codec, "png");
    assert_eq!(artwork.video.as_ref().unwrap().width, Some(32));
    assert_eq!(source.info.streams.len(), 2);
    let options = harness
        .engine()
        .media_catalog_for_source(source.id.clone(), Operation::AudioConverter)
        .unwrap();
    assert!(
        options
            .iter()
            .any(|option| option.format == OutputFormat::Flac
                && option.audio_codecs.contains(&AudioCodec::Copy)),
        "Compatible FLAC/PNG copying must remain available"
    );
    assert!(
        !options
            .iter()
            .any(|option| option.format == OutputFormat::Wav),
        "WAV cannot preserve this attached picture with the current copy-only artwork route"
    );
    let defaults = harness
        .engine()
        .default_configuration(source.id.clone(), Operation::AudioConverter)
        .unwrap();
    assert!(options
        .iter()
        .any(|option| option.format == defaults.format));
    harness
        .engine()
        .validate_configuration(source.id.clone(), defaults.clone())
        .unwrap();
    for option in &options {
        let mut config = defaults.clone();
        config.format = option.format;
        harness
            .engine()
            .validate_configuration(source.id.clone(), config)
            .unwrap_or_else(|error| {
                panic!(
                    "Advertised artwork destination {:?} is invalid: {error}",
                    option.format
                )
            });
    }
    let mut config = defaults;
    config.format = OutputFormat::Flac;
    config.audio_codec = AudioCodec::Copy;
    let completed = harness.complete(&harness.submit(&source, config).id);
    let output = harness
        .engine()
        .inspect_source(
            completed.output.as_ref().unwrap().locator.clone(),
            "published-cover.flac".into(),
            Operation::AudioConverter,
            CancellationToken::new(),
        )
        .unwrap();
    assert_eq!(output.info.streams.len(), 2);
    let copied_artwork = output
        .info
        .streams
        .iter()
        .find(|stream| {
            stream
                .video
                .as_ref()
                .is_some_and(|video| video.attached_picture)
        })
        .expect("Publication discarded the attached picture");
    assert_eq!(copied_artwork.codec, artwork.codec);
    assert_eq!(copied_artwork.video.as_ref().unwrap().width, Some(32));
    assert_eq!(copied_artwork.video.as_ref().unwrap().height, Some(32));
    harness.engine().release_inspection(output.id).unwrap();
    harness.engine().release_inspection(source.id).unwrap();
    assert_eq!(digest(&fixture("cover-art.flac")), original_hash);
    let mut expected = expectation(&completed, "flac", 2, 48000);
    expected["source"] = json!("cover-art.flac");
    expected["attached_picture_codec"] = json!("png");
    harness.expect(vec![expected]);
}

#[test]
fn complete_video_codec_and_container_matrix_encodes_real_content() {
    let harness = Harness::new("m3-video-codecs");
    let source = harness.inspect("video.mp4", Operation::VideoConverter);
    let mut cases = Vec::new();
    for (format, codec, audio, expected_video, expected_audio, container) in [
        (
            OutputFormat::Mp4,
            VideoCodec::H264,
            AudioCodec::Aac,
            "h264",
            "aac",
            "mov",
        ),
        (
            OutputFormat::Matroska,
            VideoCodec::Hevc,
            AudioCodec::Aac,
            "hevc",
            "aac",
            "matroska",
        ),
        (
            OutputFormat::Matroska,
            VideoCodec::Av1,
            AudioCodec::Opus,
            "av1",
            "opus",
            "matroska",
        ),
        (
            OutputFormat::Webm,
            VideoCodec::Vp8,
            AudioCodec::Opus,
            "vp8",
            "opus",
            "webm",
        ),
        (
            OutputFormat::Webm,
            VideoCodec::Vp9,
            AudioCodec::Vorbis,
            "vp9",
            "vorbis",
            "webm",
        ),
        (
            OutputFormat::Avi,
            VideoCodec::Mpeg4,
            AudioCodec::Mp3,
            "mpeg4",
            "mp3",
            "avi",
        ),
        (
            OutputFormat::Matroska,
            VideoCodec::Ffv1,
            AudioCodec::Flac,
            "ffv1",
            "flac",
            "matroska",
        ),
        (
            OutputFormat::Mov,
            VideoCodec::Mjpeg,
            AudioCodec::Pcm,
            "mjpeg",
            "pcm_s16le",
            "mov",
        ),
        (
            OutputFormat::M4v,
            VideoCodec::H264,
            AudioCodec::Aac,
            "h264",
            "aac",
            "mov",
        ),
        (
            OutputFormat::MpegTs,
            VideoCodec::H264,
            AudioCodec::Ac3,
            "h264",
            "ac3",
            "mpegts",
        ),
        (
            OutputFormat::Flv,
            VideoCodec::H264,
            AudioCodec::Aac,
            "h264",
            "aac",
            "flv",
        ),
        (
            OutputFormat::ThreeGp,
            VideoCodec::Mpeg4,
            AudioCodec::Aac,
            "mpeg4",
            "aac",
            "mov",
        ),
        (
            OutputFormat::Asf,
            VideoCodec::Mpeg4,
            AudioCodec::Wma,
            "mpeg4",
            "wmav2",
            "asf",
        ),
    ] {
        let mut config = harness.defaults(&source, Operation::VideoConverter);
        config.format = format;
        config.video_codec = codec;
        config.audio_codec = audio;
        if audio == AudioCodec::Pcm {
            config.audio_precision = Some(AudioPrecision::Pcm16);
        }
        config.file_name = format!("matrix_{format:?}_{codec:?}");
        let record = harness.complete(&harness.submit(&source, config).id);
        cases.push(
            json!({"path":record.output.unwrap().locator,"container":container,
            "video_codec":expected_video,"width":320,"height":180,"audio_codecs":[expected_audio],
            "channels":2,"sample_rate":48000,"duration":2.0,"tones":true,
            "source":"video.mp4","match_source_frame":true,"decoded_video_frames":48}),
        );
    }
    harness.expect(cases);
}

#[test]
fn complete_audio_codec_and_container_matrix_encodes_real_samples() {
    let harness = Harness::new("m3-audio-codecs");
    let source = harness.inspect("stereo.wav", Operation::AudioConverter);
    let mut cases = Vec::new();
    for (format, codec, expected, container) in [
        (OutputFormat::Mp3, AudioCodec::Mp3, "mp3", "mp3"),
        (OutputFormat::M4a, AudioCodec::Aac, "aac", "mov"),
        (OutputFormat::M4a, AudioCodec::Alac, "alac", "mov"),
        (OutputFormat::Flac, AudioCodec::Flac, "flac", "flac"),
        (OutputFormat::Ogg, AudioCodec::Vorbis, "vorbis", "ogg"),
        (OutputFormat::Opus, AudioCodec::Opus, "opus", "ogg"),
        (OutputFormat::Aac, AudioCodec::Aac, "aac", "aac"),
        (OutputFormat::Aiff, AudioCodec::Pcm, "pcm_s16be", "aiff"),
        (OutputFormat::M4a, AudioCodec::Ac3, "ac3", "mov"),
        (OutputFormat::M4a, AudioCodec::Eac3, "eac3", "mov"),
        (OutputFormat::Wavpack, AudioCodec::Wavpack, "wavpack", "wv"),
        (OutputFormat::Wma, AudioCodec::Wma, "wmav2", "asf"),
    ] {
        let mut config = harness.defaults(&source, Operation::AudioConverter);
        config.format = format;
        config.audio_codec = codec;
        config.file_name = format!("matrix_{format:?}_{codec:?}");
        let record = harness.complete(&harness.submit(&source, config).id);
        let mut expected = expectation(&record, expected, 2, 48000);
        expected["container"] = json!(container);
        if matches!(
            codec,
            AudioCodec::Flac | AudioCodec::Alac | AudioCodec::Pcm | AudioCodec::Wavpack
        ) {
            expected["exact_audio_source"] = json!("stereo.wav");
        }
        cases.push(expected);
    }
    harness.expect(cases);
}

#[test]
fn actual_main10_hevc_and_vp9_preserve_precision_and_color() {
    let harness = Harness::new("m3-ten-bit");
    let mut cases = Vec::new();
    for (name, format, codec, expected, transfer, primaries, matrix) in [
        (
            "camera10.mp4",
            OutputFormat::Mp4,
            VideoCodec::Hevc,
            "hevc",
            "bt709",
            "bt709",
            "bt709",
        ),
        (
            "camera10.mp4",
            OutputFormat::Webm,
            VideoCodec::Vp9,
            "vp9",
            "bt709",
            "bt709",
            "bt709",
        ),
        (
            "hdr.mkv",
            OutputFormat::Mp4,
            VideoCodec::Hevc,
            "hevc",
            "smpte2084",
            "bt2020",
            "bt2020nc",
        ),
        (
            "hlg.mkv",
            OutputFormat::Webm,
            VideoCodec::Vp9,
            "vp9",
            "arib-std-b67",
            "bt2020",
            "bt2020nc",
        ),
    ] {
        let source = harness.inspect(name, Operation::VideoConverter);
        let mut config = harness.defaults(&source, Operation::VideoConverter);
        config.format = format;
        config.video_codec = codec;
        config.file_name = format!("precision_{name}_{codec:?}");
        let record = harness.complete(&harness.submit(&source, config).id);
        cases.push(
            json!({"path":record.output.unwrap().locator,"video_codec":expected,
            "width":160,"height":90,"pixel_format":"yuv420p10le","color_transfer":transfer,
            "color_primaries":primaries,"color_space":matrix,"audio_codecs":[],"duration":1.0,
            "source":name,"decoded_video_frames":12,"ten_bit_reference":name}),
        );
    }
    harness.expect(cases);
}

#[test]
fn hdr10_static_metadata_survives_real_hevc_encoding_into_transport_stream() {
    let harness = Harness::new("m3-hdr10-sei");
    let source = harness.inspect("camera10-hdr.mp4", Operation::VideoConverter);
    let mut config = harness.defaults(&source, Operation::VideoConverter);
    config.format = OutputFormat::MpegTs;
    config.video_codec = VideoCodec::Hevc;
    config.file_name = "hdr10_reencoded_transport".into();
    let record = harness.complete(&harness.submit(&source, config).id);
    harness.expect(vec![
        json!({"path":record.output.unwrap().locator,"container":"mpegts",
        "video_codec":"hevc","width":160,"height":90,"pixel_format":"yuv420p10le",
        "color_transfer":"smpte2084","color_primaries":"bt2020","color_space":"bt2020nc",
        "audio_codecs":[],"duration":1.0,"source":"camera10-hdr.mp4","decoded_video_frames":12,
        "ten_bit_reference":"camera10-hdr.mp4","hdr10_elementary_sei":true,"reencoded_video":true}),
    ]);
}

#[test]
fn pcm_precisions_and_lossless_multichannel_preserve_real_samples() {
    let harness = Harness::new("m3-audio-precision");
    let source = harness.inspect("precision24.wav", Operation::AudioConverter);
    let mut cases = Vec::new();
    for (precision, codec, bits) in [
        (AudioPrecision::Pcm8, "pcm_u8", 8),
        (AudioPrecision::Pcm16, "pcm_s16le", 16),
        (AudioPrecision::Pcm24, "pcm_s24le", 24),
        (AudioPrecision::Pcm32, "pcm_s32le", 32),
        (AudioPrecision::Float32, "pcm_f32le", 32),
        (AudioPrecision::Float64, "pcm_f64le", 64),
    ] {
        let mut config = harness.defaults(&source, Operation::AudioConverter);
        config.format = OutputFormat::Wav;
        config.audio_codec = AudioCodec::Pcm;
        config.audio_precision = Some(precision);
        config.channel_layout = Some(AudioLayout::Surround51);
        config.file_name = format!("pcm_{precision:?}");
        let record = harness.complete(&harness.submit(&source, config).id);
        cases.push(json!({"path":record.output.unwrap().locator,"container":"wav","audio_codecs":[codec],
            "channels":6,"channel_layout":"5.1","sample_rate":48000,"audio_bits":bits,"duration":2.0,
            "decoded_samples":96000,"tones":true,"tone_frequencies":[220,330,440,55,660,880],
            "pcm_precision_source":"precision24.wav","pcm_quantization_bits":if bits==8 {8} else if bits==16 {16} else {24}}));
    }
    for (format, codec, expected) in [
        (OutputFormat::Flac, AudioCodec::Flac, "flac"),
        (OutputFormat::M4a, AudioCodec::Alac, "alac"),
        (OutputFormat::Aiff, AudioCodec::Pcm, "pcm_s24be"),
    ] {
        let mut config = harness.defaults(&source, Operation::AudioConverter);
        config.format = format;
        config.audio_codec = codec;
        config.audio_precision = Some(AudioPrecision::Pcm24);
        config.channel_layout = Some(AudioLayout::Surround51);
        config.file_name = format!("surround_{format:?}");
        let record = harness.complete(&harness.submit(&source, config).id);
        cases.push(
            json!({"path":record.output.unwrap().locator,"audio_codecs":[expected],"channels":6,
            "channel_layout":"5.1","sample_rate":48000,"audio_bits":24,"duration":2.0,
            "decoded_samples":96000,"tones":true,"tone_frequencies":[220,330,440,55,660,880],
            "exact_audio_source":"precision24.wav"}),
        );
    }
    harness.expect(cases);
}

#[test]
fn incompatible_catalog_requests_are_rejected_directly_by_the_core() {
    let harness = Harness::new("m3-codec-validation");
    let video = harness.inspect("video.mp4", Operation::VideoConverter);
    for (format, codec, audio) in [
        (OutputFormat::Webm, VideoCodec::H264, AudioCodec::Opus),
        (OutputFormat::Mp4, VideoCodec::Vp8, AudioCodec::Aac),
        (OutputFormat::Flv, VideoCodec::Hevc, AudioCodec::Aac),
        (OutputFormat::ThreeGp, VideoCodec::Ffv1, AudioCodec::Aac),
        (OutputFormat::Webm, VideoCodec::Vp9, AudioCodec::Aac),
    ] {
        let mut config = harness.defaults(&video, Operation::VideoConverter);
        config.format = format;
        config.video_codec = codec;
        config.audio_codec = audio;
        assert!(matches!(
            harness.engine().submit_conversion(
                video.id.clone(),
                uuid::Uuid::new_v4().to_string(),
                config,
                CancellationToken::new()
            ),
            Err(EngineError::Configuration { .. })
        ));
    }
    let audio = harness.inspect("stereo.wav", Operation::AudioConverter);
    for (format, codec) in [
        (OutputFormat::Mp3, AudioCodec::Flac),
        (OutputFormat::Flac, AudioCodec::Aac),
        (OutputFormat::Aac, AudioCodec::HeAac),
        (OutputFormat::Amr, AudioCodec::AmrNb),
        (OutputFormat::Amr, AudioCodec::AmrWb),
    ] {
        let mut config = harness.defaults(&audio, Operation::AudioConverter);
        config.format = format;
        config.audio_codec = codec;
        assert!(matches!(
            harness.engine().submit_conversion(
                audio.id.clone(),
                uuid::Uuid::new_v4().to_string(),
                config,
                CancellationToken::new()
            ),
            Err(EngineError::Configuration { .. } | EngineError::UnsupportedMedia { .. })
        ));
    }
    assert!(harness.engine().jobs().unwrap().is_empty());
}

#[test]
fn original_automatic_keeps_compatible_video_and_audio_payloads() {
    let harness = Harness::new("m3-original-copy");
    let video = harness.inspect("video.mp4", Operation::VideoConverter);
    let mut config = harness.defaults(&video, Operation::VideoConverter);
    config.format = OutputFormat::Original;
    config.video_codec = VideoCodec::Automatic;
    config.audio_codec = AudioCodec::Automatic;
    let video_job = harness.complete(&harness.submit(&video, config).id);
    assert!(video_job.file_name.ends_with(".mp4"));
    let audio = harness.inspect("stereo.flac", Operation::AudioConverter);
    let mut config = harness.defaults(&audio, Operation::AudioConverter);
    config.format = OutputFormat::Original;
    config.audio_codec = AudioCodec::Automatic;
    let audio_job = harness.complete(&harness.submit(&audio, config).id);
    assert!(audio_job.file_name.ends_with(".flac"));
    harness.expect(vec![
        json!({"path":video_job.output.unwrap().locator,"container":"mov","video_codec":"h264",
            "width":320,"height":180,"audio_codecs":["aac"],"channels":2,"sample_rate":48000,
            "duration":2.0,"source":"video.mp4","preserve_video_packets":true,"preserve_audio_packets":true}),
        json!({"path":audio_job.output.unwrap().locator,"container":"flac","audio_codecs":["flac"],
            "channels":2,"sample_rate":48000,"duration":2.0,"source":"stereo.flac",
            "preserve_audio_packets":true,"exact_audio_source":"stereo.flac"}),
    ]);
}

#[test]
fn every_advertised_explicit_codec_and_container_pair_produces_verified_media() {
    fn container(format: OutputFormat) -> &'static str {
        match format {
            OutputFormat::Mp4
            | OutputFormat::Mov
            | OutputFormat::M4v
            | OutputFormat::ThreeGp
            | OutputFormat::M4a => "mov",
            OutputFormat::Matroska => "matroska",
            OutputFormat::Webm => "webm",
            OutputFormat::Avi => "avi",
            OutputFormat::MpegTs => "mpegts",
            OutputFormat::Flv => "flv",
            OutputFormat::Asf | OutputFormat::Wma => "asf",
            OutputFormat::Mp3 => "mp3",
            OutputFormat::Wav => "wav",
            OutputFormat::Flac => "flac",
            OutputFormat::Ogg | OutputFormat::Opus => "ogg",
            OutputFormat::Aac => "aac",
            OutputFormat::Aiff => "aiff",
            OutputFormat::Amr => "amr",
            OutputFormat::Wavpack => "wv",
            OutputFormat::Original => panic!("Original has dedicated copy tests"),
        }
    }
    fn video_name(codec: VideoCodec) -> &'static str {
        match codec {
            VideoCodec::H264 => "h264",
            VideoCodec::Hevc => "hevc",
            VideoCodec::Av1 => "av1",
            VideoCodec::Vp8 => "vp8",
            VideoCodec::Vp9 => "vp9",
            VideoCodec::Mpeg4 => "mpeg4",
            VideoCodec::Ffv1 => "ffv1",
            VideoCodec::Mjpeg => "mjpeg",
            VideoCodec::Automatic | VideoCodec::Copy => panic!("An explicit encoder is required"),
        }
    }
    fn audio_name(codec: AudioCodec, format: OutputFormat) -> &'static str {
        match codec {
            AudioCodec::Aac | AudioCodec::HeAac => "aac",
            AudioCodec::Mp3 => "mp3",
            AudioCodec::Pcm if format == OutputFormat::Aiff => "pcm_s16be",
            AudioCodec::Pcm | AudioCodec::PcmS16le => "pcm_s16le",
            AudioCodec::Flac => "flac",
            AudioCodec::Alac => "alac",
            AudioCodec::Opus => "opus",
            AudioCodec::Vorbis => "vorbis",
            AudioCodec::Ac3 => "ac3",
            AudioCodec::Eac3 => "eac3",
            AudioCodec::Wavpack => "wavpack",
            AudioCodec::Wma => "wmav2",
            AudioCodec::AmrNb => "amr_nb",
            AudioCodec::AmrWb => "amr_wb",
            AudioCodec::Automatic | AudioCodec::Copy => panic!("An explicit encoder is required"),
        }
    }

    let harness = Harness::new("m3-advertised-pairs");
    let video = harness.inspect("video.mp4", Operation::VideoConverter);
    let audio = harness.inspect("stereo.wav", Operation::AudioConverter);
    let mut cases = Vec::new();
    let mut video_formats = Vec::new();
    let mut video_codecs = Vec::new();
    let mut audio_formats = Vec::new();
    let mut audio_codecs = Vec::new();
    for option in harness
        .engine()
        .media_catalog_for_source(video.id.clone(), Operation::VideoConverter)
        .unwrap()
    {
        if option.format == OutputFormat::Original {
            continue;
        }
        let audio_can_copy = option.audio_codecs.contains(&AudioCodec::Copy);
        for codec in option
            .video_codecs
            .into_iter()
            .filter(|codec| !matches!(codec, VideoCodec::Automatic | VideoCodec::Copy))
        {
            let mut config = harness.defaults(&video, Operation::VideoConverter);
            config.format = option.format;
            config.video_codec = codec;
            config.audio_codec = AudioCodec::Automatic;
            config.file_name = format!("advertised_{:?}_{codec:?}", option.format);
            let result = harness.complete(&harness.submit(&video, config).id);
            // Automatic must preserve the fixture's AAC whenever the source
            // catalog advertises compatible copying, including legacy muxers.
            let expected_audio = if audio_can_copy {
                "aac"
            } else {
                match option.format {
                    OutputFormat::Webm => "opus",
                    OutputFormat::Avi => "mp3",
                    OutputFormat::Asf => "wmav2",
                    _ => "aac",
                }
            };
            cases.push(json!({"path":result.output.unwrap().locator,"container":container(option.format),
                "video_codec":video_name(codec),"width":320,"height":180,"audio_codecs":[expected_audio],
                "channels":2,"sample_rate":48000,"duration":2.0,"tones":true,"source":"video.mp4",
                "decoded_video_frames":48,"match_source_frame":true}));
            if !video_formats.contains(&option.format) {
                video_formats.push(option.format);
            }
            if !video_codecs.contains(&codec) {
                video_codecs.push(codec);
            }
        }
    }
    for option in harness
        .engine()
        .media_catalog_for_source(audio.id.clone(), Operation::AudioConverter)
        .unwrap()
    {
        if option.format == OutputFormat::Original {
            continue;
        }
        for codec in option
            .audio_codecs
            .into_iter()
            .filter(|codec| !matches!(codec, AudioCodec::Automatic | AudioCodec::Copy))
        {
            let mut config = harness.defaults(&audio, Operation::AudioConverter);
            config.format = option.format;
            config.audio_codec = codec;
            config.file_name = format!("advertised_{:?}_{codec:?}", option.format);
            let (channels, rate) = match codec {
                AudioCodec::AmrNb => (1, 8000),
                AudioCodec::AmrWb => (1, 16000),
                _ => (2, 48000),
            };
            if channels == 1 {
                config.channels = Some(channels);
                config.sample_rate = Some(rate);
            }
            let result = harness.complete(&harness.submit(&audio, config).id);
            let mut expected =
                expectation(&result, audio_name(codec, option.format), channels, rate);
            expected["container"] = json!(container(option.format));
            if matches!(
                codec,
                AudioCodec::Flac
                    | AudioCodec::Alac
                    | AudioCodec::Pcm
                    | AudioCodec::PcmS16le
                    | AudioCodec::Wavpack
            ) {
                expected["exact_audio_source"] = json!("stereo.wav");
            }
            cases.push(expected);
            if !audio_formats.contains(&option.format) {
                audio_formats.push(option.format);
            }
            if !audio_codecs.contains(&codec) {
                audio_codecs.push(codec);
            }
        }
    }
    assert_eq!(
        video_formats.len(),
        10,
        "An unconditional video representation lacks actual encoder coverage: {video_formats:?}"
    );
    assert_eq!(
        video_codecs.len(),
        8,
        "An unconditional video encoder lacks actual coverage: {video_codecs:?}"
    );
    assert!(
        audio_formats.len() >= 10,
        "An unconditional audio representation lacks actual encoder coverage: {audio_formats:?}"
    );
    assert!(
        audio_codecs.len() >= 11,
        "An unconditional audio encoder lacks actual coverage: {audio_codecs:?}"
    );
    harness.expect(cases);
}

fn compression_settings(mode: CompressionMode) -> CompressionSettings {
    CompressionSettings {
        reduce_file_size: true,
        mode,
        ..Default::default()
    }
}

fn accepted_request_text(harness: &Harness, id: &str) -> String {
    Connection::open(harness.root.join("operational/operations.sqlite3"))
        .unwrap()
        .query_row("SELECT request FROM jobs WHERE id=?1", [id], |row| {
            row.get(0)
        })
        .unwrap()
}

fn after_worker_ack(mut action: impl FnMut() -> Result<(), EngineError>) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match action() {
            Ok(()) => return,
            Err(EngineError::Configuration { detail })
                if detail.starts_with("Wait for") && Instant::now() < deadline =>
            {
                thread::sleep(Duration::from_millis(5));
            }
            result => panic!("Worker-acknowledged decision failed: {result:?}"),
        }
    }
}

fn target_miss(harness: &Harness, source: &InspectedSource) -> JobRecord {
    let mut config = harness.defaults(source, Operation::VideoConverter);
    config.video_codec = VideoCodec::Mjpeg;
    config.compression = Some(CompressionSettings {
        target: Some(TargetSize::parse_decimal("8", TargetSizeUnit::Kilobytes).unwrap()),
        ..compression_settings(CompressionMode::TargetSize)
    });
    let before = harness.provider.open_calls.load(Ordering::SeqCst);
    let job = harness.submit(source, config);
    let result = harness.wait(&job.id, |record| {
        matches!(
            record.state,
            JobState::TargetNotMet | JobState::Failed | JobState::Completed
        )
    });
    assert_eq!(result.state, JobState::TargetNotMet, "{result:?}");
    assert!(result.verified && result.verified_size.unwrap() > 8_000);
    assert!(result.reservation.is_none() && result.output.is_none());
    assert_eq!(result.error_category.as_deref(), Some("target_not_met"));
    assert!(!result.keep_target_miss);
    assert!(
        harness.provider.open_calls.load(Ordering::SeqCst) - before <= 3,
        "Submission plus processing must use at most two real conversion source opens"
    );
    let parent = harness.work(&result).parent().unwrap().to_path_buf();
    assert!(
        fs::read_dir(parent).unwrap().all(|entry| !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with("corrective-")),
        "Corrective scratch survived worker return"
    );
    result
}

#[test]
fn shared_compression_presets_custom_controls_and_copying_process_real_media() {
    let harness = Harness::new("m3-compression-controls");
    let source = harness.inspect("video.mp4", Operation::VideoConverter);
    let original_hash = digest(&fixture("video.mp4"));
    let mut config = harness.defaults(&source, Operation::VideoConverter);
    config.compression = Some(CompressionSettings::default());
    config.file_name = "compression_disabled_copy".into();
    let copied = harness.complete(&harness.submit(&source, config.clone()).id);
    config.compression = Some(compression_settings(CompressionMode::HighQuality));
    config.file_name = "compression_high".into();
    let higher = harness.complete(&harness.submit(&source, config.clone()).id);
    config.compression = Some(compression_settings(CompressionMode::SmallerFile));
    config.file_name = "compression_smaller".into();
    let smaller = harness.complete(&harness.submit(&source, config.clone()).id);
    assert!(smaller.verified_size < higher.verified_size);
    config.compression = Some(CompressionSettings {
        custom_audio: Some(AudioRateControl::AverageBitrate {
            bits_per_second: 96_000,
        }),
        ..compression_settings(CompressionMode::Custom)
    });
    config.file_name = "custom_audio_preserves_video".into();
    let audio_edit = harness.complete(&harness.submit(&source, config).id);
    let video_case = |record: &JobRecord| {
        json!({"path":record.output.as_ref().unwrap().locator,
        "video_codec":"h264","width":320,"height":180,"audio_codecs":["aac"],
        "channels":2,"sample_rate":48000,"tones":true,"duration":2.0})
    };
    let mut copy_case = video_case(&copied);
    copy_case["source"] = json!("video.mp4");
    copy_case["preserve_video_packets"] = json!(true);
    copy_case["preserve_audio_packets"] = json!(true);
    let mut high_case = video_case(&higher);
    high_case["quality_reference"] = json!(fixture("video.mp4"));
    high_case["lower_quality_output"] = json!(smaller.output.as_ref().unwrap().locator);
    let mut audio_edit_case = video_case(&audio_edit);
    audio_edit_case["source"] = json!("video.mp4");
    audio_edit_case["preserve_video_packets"] = json!(true);

    let audio = harness.inspect("stereo.wav", Operation::AudioConverter);
    let mut config = harness.defaults(&audio, Operation::AudioConverter);
    config.format = OutputFormat::Opus;
    config.audio_codec = AudioCodec::Opus;
    config.compression = Some(CompressionSettings {
        custom_audio: Some(AudioRateControl::AverageBitrate {
            bits_per_second: 32_000,
        }),
        ..compression_settings(CompressionMode::Custom)
    });
    config.file_name = "custom_opus_32".into();
    let lower = harness.complete(&harness.submit(&audio, config.clone()).id);
    config.compression.as_mut().unwrap().custom_audio = Some(AudioRateControl::AverageBitrate {
        bits_per_second: 128_000,
    });
    config.file_name = "custom_opus_128".into();
    let upper = harness.complete(&harness.submit(&audio, config).id);
    assert!(lower.verified_size < upper.verified_size);
    let lossless = harness.inspect("stereo.flac", Operation::AudioConverter);
    let mut config = harness.defaults(&lossless, Operation::AudioConverter);
    config.format = OutputFormat::Flac;
    config.compression = Some(CompressionSettings {
        custom_audio: Some(AudioRateControl::FlacLevel { value: 12 }),
        ..compression_settings(CompressionMode::Custom)
    });
    let flac = harness.complete(&harness.submit(&lossless, config).id);
    let accepted: Value = serde_json::from_str(&accepted_request_text(&harness, &flac.id)).unwrap();
    assert_eq!(
        accepted["plan"]["streams"][0]["encoder"], "flac",
        "Explicit lossless effort must not be ignored by copying"
    );
    assert!(accepted["plan"]["streams"][0]["encoding"]["options"]
        .as_array()
        .unwrap()
        .iter()
        .any(|option| option["key"] == "compression_level" && option["value"] == "12"));
    let mut flac_case = expectation(&flac, "flac", 2, 48000);
    flac_case["exact_audio_source"] = json!("stereo.flac");
    assert_eq!(digest(&fixture("video.mp4")), original_hash);
    harness.expect(vec![
        copy_case,
        high_case,
        video_case(&smaller),
        audio_edit_case,
        expectation(&lower, "opus", 2, 48000),
        expectation(&upper, "opus", 2, 48000),
        flac_case,
    ]);
}

#[test]
fn target_size_audio_encoding_publishes_only_verified_actual_bytes() {
    let harness = Harness::new("m3-target-audio");
    let source = harness.inspect("stereo.wav", Operation::AudioConverter);
    let mut config = harness.defaults(&source, Operation::AudioConverter);
    config.format = OutputFormat::Mp3;
    config.audio_codec = AudioCodec::Mp3;
    config.compression = Some(CompressionSettings {
        target: Some(TargetSize::parse_decimal("32", TargetSizeUnit::Kilobytes).unwrap()),
        ..compression_settings(CompressionMode::TargetSize)
    });
    let result = harness.complete(&harness.submit(&source, config).id);
    assert!(result.verified_size.unwrap() <= 32_000);
    assert!(!result.keep_target_miss);
    assert_eq!(
        fs::metadata(&result.output.as_ref().unwrap().locator)
            .unwrap()
            .len(),
        result.verified_size.unwrap()
    );
    let mut expected = expectation(&result, "mp3", 2, 48000);
    expected["gapless_duration"] = json!(true);
    expected["decoded_samples"] = json!(96_000);
    harness.expect(vec![expected]);
}

#[test]
fn target_not_met_survives_restart_and_keep_requires_explicit_verified_save() {
    let mut harness = Harness::new("m3-target-keep");
    let source = harness.inspect("video-large.mp4", Operation::VideoConverter);
    let original_hash = digest(&fixture("video-large.mp4"));
    let miss = target_miss(&harness, &source);
    let work = harness.work(&miss);
    let verified_hash = digest(&work);
    let accepted = accepted_request_text(&harness, &miss.id);
    harness.engine().release_inspection(source.id).unwrap();
    assert_eq!(harness.provider.publish_calls.load(Ordering::SeqCst), 0);
    assert!(harness
        .provider
        .retained_sources
        .lock()
        .unwrap()
        .contains(&source.locator));
    harness.close();
    harness.reopen();
    assert_eq!(harness.record(&miss.id).state, JobState::TargetNotMet);
    assert_eq!(digest(&work), verified_hash);
    assert_eq!(accepted_request_text(&harness, &miss.id), accepted);
    assert_eq!(harness.provider.publish_calls.load(Ordering::SeqCst), 0);
    after_worker_ack(|| harness.engine().keep_target_output(miss.id.clone()));
    harness
        .engine()
        .keep_target_output(miss.id.clone())
        .unwrap();
    let saved = harness.complete(&miss.id);
    assert!(saved.keep_target_miss);
    assert!(saved.verified_size.unwrap() > 8_000);
    assert_eq!(accepted_request_text(&harness, &miss.id), accepted);
    assert_eq!(
        digest(Path::new(&saved.output.as_ref().unwrap().locator)),
        verified_hash
    );
    assert!(!work.exists());
    assert!(!harness
        .provider
        .retained_sources
        .lock()
        .unwrap()
        .contains(&source.locator));
    assert_eq!(digest(&fixture("video-large.mp4")), original_hash);
    harness.expect(vec![
        json!({"path":saved.output.unwrap().locator,"video_codec":"mjpeg",
        "width":960,"height":540,"audio_codecs":[],"duration":1.0}),
    ]);
}

#[test]
fn adjusted_target_is_a_new_idempotent_request_and_old_output_requires_discard() {
    let harness = Harness::new("m3-target-adjust");
    let source = harness.inspect("video-large.mp4", Operation::VideoConverter);
    let miss = target_miss(&harness, &source);
    let old_request = accepted_request_text(&harness, &miss.id);
    let old_work = harness.work(&miss);
    let old_hash = digest(&old_work);
    let key = uuid::Uuid::new_v4().to_string();
    let target = Some(TargetSize::parse_decimal("100", TargetSizeUnit::Megabytes).unwrap());
    let deadline = Instant::now() + Duration::from_secs(10);
    let new_job = loop {
        match harness.engine().resubmit_target(
            miss.id.clone(),
            target,
            key.clone(),
            CancellationToken::new(),
        ) {
            Ok(record) => break record,
            Err(EngineError::Configuration { detail })
                if detail.starts_with("Wait for") && Instant::now() < deadline =>
            {
                thread::sleep(Duration::from_millis(5))
            }
            result => panic!("Target retry failed: {result:?}"),
        }
    };
    assert_ne!(new_job.id, miss.id);
    let repeated = harness
        .engine()
        .resubmit_target(
            miss.id.clone(),
            target,
            key.clone(),
            CancellationToken::new(),
        )
        .unwrap();
    assert_eq!(repeated.id, new_job.id);
    assert_eq!(accepted_request_text(&harness, &miss.id), old_request);
    assert_eq!(digest(&old_work), old_hash);
    let completed = harness.complete(&new_job.id);
    let published = completed.output.as_ref().unwrap().locator.clone();
    assert_eq!(harness.record(&miss.id).state, JobState::TargetNotMet);
    harness
        .engine()
        .discard_target_output(miss.id.clone())
        .unwrap();
    harness
        .engine()
        .discard_target_output(miss.id.clone())
        .unwrap();
    assert_eq!(harness.record(&miss.id).state, JobState::Canceled);
    assert!(!old_work.exists());
    assert!(Path::new(&published).is_file());
    assert_eq!(accepted_request_text(&harness, &miss.id), old_request);
    assert_eq!(
        harness
            .engine()
            .resubmit_target(miss.id, target, key, CancellationToken::new())
            .unwrap()
            .id,
        new_job.id
    );
    assert!(
        harness
            .provider
            .maximum_worker_sources
            .load(Ordering::SeqCst)
            <= 1
    );
    harness.expect(vec![json!({"path":published,"video_codec":"mjpeg","width":960,"height":540,"audio_codecs":[],"duration":1.0})]);
}

#[test]
fn invalid_compression_and_infeasible_targets_are_rejected_by_rust() {
    let harness = Harness::new("m3-compression-validation");
    let video = harness.inspect("video.mp4", Operation::VideoConverter);
    let base = harness.defaults(&video, Operation::VideoConverter);
    let mut invalid = Vec::new();
    let mut config = base.clone();
    config.video_codec = VideoCodec::H264;
    config.compression = Some(CompressionSettings {
        custom_video: Some(VideoRateControl::HevcCrf { value: 20 }),
        ..compression_settings(CompressionMode::Custom)
    });
    invalid.push(config);
    let mut config = base.clone();
    config.video_size = VideoSize::Hd720;
    config.compression = Some(CompressionSettings {
        optimize_resolution: true,
        ..compression_settings(CompressionMode::Balanced)
    });
    invalid.push(config);
    let mut config = base.clone();
    config.quality = 28;
    config.compression = Some(compression_settings(CompressionMode::Balanced));
    invalid.push(config);
    let mut config = base.clone();
    config.video_codec = VideoCodec::Copy;
    config.compression = Some(compression_settings(CompressionMode::SmallerFile));
    invalid.push(config);
    let mut config = base;
    config.compression = Some(CompressionSettings {
        target: Some(TargetSize::parse_decimal("1", TargetSizeUnit::Kilobytes).unwrap()),
        ..compression_settings(CompressionMode::TargetSize)
    });
    invalid.push(config);
    for config in invalid {
        assert!(matches!(
            harness.engine().submit_conversion(
                video.id.clone(),
                uuid::Uuid::new_v4().to_string(),
                config,
                CancellationToken::new()
            ),
            Err(EngineError::Configuration { .. })
        ));
    }
    let audio = harness.inspect("stereo.wav", Operation::AudioConverter);
    let mut config = harness.defaults(&audio, Operation::AudioConverter);
    config.audio_codec = AudioCodec::Aac;
    config.compression = Some(CompressionSettings {
        custom_audio: Some(AudioRateControl::Mp3VbrQuality { value: 2 }),
        ..compression_settings(CompressionMode::Custom)
    });
    assert!(matches!(
        harness.engine().submit_conversion(
            audio.id,
            uuid::Uuid::new_v4().to_string(),
            config,
            CancellationToken::new()
        ),
        Err(EngineError::Configuration { .. })
    ));
    assert!(harness.engine().jobs().unwrap().is_empty());
}
