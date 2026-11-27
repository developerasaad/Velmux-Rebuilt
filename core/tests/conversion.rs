//! Real media integration. CLI fixture generation and independent output checks
//! live outside the engine; this provider only implements host filesystem facilities.
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
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
    block_source: AtomicBool,
    block_publication: AtomicBool,
    block_preview: AtomicBool,
    fail_publications: AtomicUsize,
    fail_after_publish: AtomicBool,
    open_calls: AtomicUsize,
    worker_sources: AtomicUsize,
    maximum_worker_sources: AtomicUsize,
    publish_calls: AtomicUsize,
    leases: Mutex<HashMap<String, bool>>,
    publication_lock: Mutex<()>,
}
impl ProviderState {
    fn new(root: PathBuf) -> Arc<Self> {
        Arc::new(Self {
            root,
            source_gate: Gate::default(),
            publication_gate: Gate::default(),
            preview_gate: Gate::default(),
            block_source: AtomicBool::new(false),
            block_publication: AtomicBool::new(false),
            block_preview: AtomicBool::new(false),
            fail_publications: AtomicUsize::new(0),
            fail_after_publish: AtomicBool::new(false),
            open_calls: AtomicUsize::new(0),
            worker_sources: AtomicUsize::new(0),
            maximum_worker_sources: AtomicUsize::new(0),
            publish_calls: AtomicUsize::new(0),
            leases: Mutex::new(HashMap::new()),
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
        self.0.leases.lock().unwrap().insert(lease.clone(), worker);
        Ok(OpenedSource {
            fd: file.into_raw_fd(),
            offset: 0,
            length: Some(length),
            lease_id: lease,
        })
    }
    fn release_source(&self, lease_id: String) {
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
    harness.expect(vec![json!({"path":record.output.unwrap().locator,"video_codec":"h264","width":852,"height":480,"audio_codecs":[],"duration":1.0})]);
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
        encode.audio_codec = AudioCodec::Aac;
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
    let scratch = harness.root.join("operational/jobs").join(&job.id);
    fs::create_dir_all(&scratch).unwrap();
    fs::write(scratch.join("in_use"), b"worker-owned marker").unwrap();
    harness.engine().cancel_job(job.id.clone()).unwrap();
    assert_eq!(harness.record(&job.id).state, JobState::Canceling);
    assert!(
        scratch.join("in_use").exists(),
        "Cancellation deleted resources before worker acknowledgement"
    );
    harness.provider.source_gate.release();
    assert_eq!(harness.finished(&job.id).state, JobState::Canceled);
    let deadline = Instant::now() + Duration::from_secs(2);
    while scratch.exists() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(5));
    }
    assert!(!scratch.exists());
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
    let replacement = harness
        .engine()
        .prepare_preview(source.id.clone(), 500_000, replacement_token.clone())
        .unwrap();
    assert!(
        obsolete.is_cancelled(),
        "A superseded preview continued without cancellation"
    );
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
    harness.provider.preview_gate.release();
    assert!(matches!(
        preparation.join().unwrap(),
        Err(EngineError::Cancelled)
    ));
    assert!(harness.provider.leases.lock().unwrap().is_empty());
    let remaining: Vec<_> = fs::read_dir(replacement_path.parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(remaining, vec![unidentified]);
    assert!(harness.engine().jobs().unwrap().is_empty());
}
