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
    AudioCodec, CancellationToken, ConversionConfig, Engine, EngineError, FrameRate,
    InspectedSource, JobObserver, JobRecord, JobState, MediaProvider, NativeProvider, OpenedSource,
    Operation, OutputFormat, OutputReservation, PublishedOutput, StreamKind, VideoCodec, VideoSize,
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
        self.engine()
            .default_configuration(source.id.clone(), operation)
            .unwrap()
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
    let tracks_default = harness.defaults(&tracks, Operation::VideoConverter);
    assert_eq!(tracks_default.format, OutputFormat::Matroska);
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
    let mut config = harness.defaults(&hdr, Operation::VideoConverter);
    assert_eq!(config.format, OutputFormat::Matroska);
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
            .args(["--exact", TEST_NAME, "--test-threads=1"])
            .env(CHILD_ENV, "1")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::inherit())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(15);
    let phase_path = root.join("phase.json");
    while !phase_path.exists() {
        assert!(
            child.0.try_wait().unwrap().is_none(),
            "Durability child exited before accepting both jobs"
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
    assert!(!harness
        .engine()
        .media_catalog_for_source(pcm32.id.clone(), Operation::AudioConverter)
        .unwrap()
        .iter()
        .any(|format| format.format == OutputFormat::Flac));
    let mut invalid_precision = harness.defaults(&pcm32, Operation::AudioConverter);
    invalid_precision.format = OutputFormat::Flac;
    assert!(matches!(
        harness
            .engine()
            .validate_configuration(pcm32.id, invalid_precision),
        Err(EngineError::Configuration { .. })
    ));

    let odd = harness.inspect("odd-video.mkv", Operation::VideoConverter);
    let odd_default = harness.defaults(&odd, Operation::VideoConverter);
    assert_eq!(odd_default.format, OutputFormat::Matroska);
    let odd_choices = harness
        .engine()
        .configuration_choices(odd.id.clone(), odd_default.clone())
        .unwrap();
    assert_eq!(odd_choices.frame_rates, vec![FrameRate::Original]);
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
    let mut config = harness.defaults(&high, Operation::AudioConverter);
    assert_eq!(config.format, OutputFormat::Flac, "Default should retain a compatible representation instead of assuming AAC supports 192 kHz");
    config.format = OutputFormat::M4a;
    let options = harness
        .engine()
        .configuration_choices(high.id.clone(), config.clone())
        .unwrap();
    assert!(!options.sample_rates.contains(&0));
    assert_eq!(options.sample_rates, vec![44100, 48000]);
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
    assert_eq!(
        options.audio_bitrates,
        vec![0],
        "Explicit exposed bitrates exceed MPEG-2.5 support at 8 kHz"
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
    let config = harness.defaults(&source, Operation::VideoConverter);
    assert_eq!(config.format, OutputFormat::Matroska);
    let catalog = harness
        .engine()
        .media_catalog_for_source(source.id.clone(), Operation::VideoConverter)
        .unwrap();
    assert_eq!(catalog.len(), 1);
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
