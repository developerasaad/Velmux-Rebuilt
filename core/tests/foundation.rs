use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc, Barrier,
};
use velmux_core::{CancellationToken, Engine, EngineError, NativeProvider};

struct Provider {
    root: String,
    callbacks: Arc<AtomicU64>,
    drops: Arc<AtomicU64>,
    cancellation: Option<Arc<CancellationToken>>,
}
impl NativeProvider for Provider {
    fn storage_root(&self) -> String {
        self.root.clone()
    }
    fn platform_name(&self) -> String {
        "host-test".into()
    }
    fn on_diagnostic(&self, _sequence: u64) {
        self.callbacks.fetch_add(1, Ordering::Relaxed);
        if let Some(token) = &self.cancellation {
            token.cancel();
        }
    }
}
impl Drop for Provider {
    fn drop(&mut self) {
        self.drops.fetch_add(1, Ordering::Relaxed);
    }
}
fn engine(
    root: &str,
    token: Option<Arc<CancellationToken>>,
) -> (Arc<Engine>, Arc<AtomicU64>, Arc<AtomicU64>) {
    let callbacks = Arc::new(AtomicU64::new(0));
    let drops = Arc::new(AtomicU64::new(0));
    let engine = Engine::new(Box::new(Provider {
        root: root.into(),
        callbacks: callbacks.clone(),
        drops: drops.clone(),
        cancellation: token,
    }));
    (engine, callbacks, drops)
}

#[test]
fn real_libraries_records_callbacks_and_shared_lifetime() {
    let (engine, callbacks, drops) = engine("/private/velmux-test", None);
    let shared = engine.clone();
    let first = engine.diagnostics(CancellationToken::new()).unwrap();
    assert!(first.ffmpeg_version.starts_with("7.1."));
    assert!(first.encoder_count > 100 && first.decoder_count > 100);
    assert!(first.muxer_count > 50 && first.demuxer_count > 50 && first.filter_count > 100);
    assert_eq!(first.storage_root, "/private/velmux-test");
    drop(engine);
    assert_eq!(drops.load(Ordering::Relaxed), 0);
    assert_eq!(
        shared
            .diagnostics(CancellationToken::new())
            .unwrap()
            .sequence,
        first.sequence + 1
    );
    assert_eq!(callbacks.load(Ordering::Relaxed), 2);
    drop(shared);
    assert_eq!(drops.load(Ordering::Relaxed), 1);
}

#[test]
fn cancellation_is_typed_and_checked_after_native_callback() {
    let token = CancellationToken::new();
    let (engine, callbacks, _) = engine("/private/velmux-test", Some(token.clone()));
    assert!(matches!(
        engine.diagnostics(token.clone()),
        Err(EngineError::Cancelled)
    ));
    assert_eq!(callbacks.load(Ordering::Relaxed), 1);
    assert!(matches!(
        engine.diagnostics(token),
        Err(EngineError::Cancelled)
    ));
    assert_eq!(callbacks.load(Ordering::Relaxed), 1);
}

#[test]
fn invalid_native_storage_does_not_report_success() {
    let (engine, callbacks, _) = engine("relative/unsafe", None);
    assert!(matches!(
        engine.diagnostics(CancellationToken::new()),
        Err(EngineError::InvalidStorageRoot)
    ));
    assert_eq!(callbacks.load(Ordering::Relaxed), 0);
}

#[test]
fn concurrent_calls_have_separate_cursors_and_one_engine() {
    let (engine, callbacks, _) = engine("/private/velmux-test", None);
    let barrier = Arc::new(Barrier::new(4));
    let threads: Vec<_> = (0..4)
        .map(|_| {
            let engine = engine.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                engine.diagnostics(CancellationToken::new()).unwrap()
            })
        })
        .collect();
    let mut reports: Vec<_> = threads
        .into_iter()
        .map(|thread| thread.join().unwrap())
        .collect();
    reports.sort_by_key(|report| report.sequence);
    assert_eq!(
        reports.iter().map(|r| r.sequence).collect::<Vec<_>>(),
        vec![1, 2, 3, 4]
    );
    assert!(reports
        .iter()
        .all(|r| r.filter_count == reports[0].filter_count));
    assert_eq!(callbacks.load(Ordering::Relaxed), 4);
}
