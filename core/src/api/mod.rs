use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc,
};

#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum EngineError {
    #[error("The request was canceled")]
    Cancelled,
    #[error("The native host did not supply an absolute private storage directory")]
    InvalidStorageRoot,
}

#[uniffi::export(callback_interface)]
pub trait NativeProvider: Send + Sync {
    fn storage_root(&self) -> String;
    fn platform_name(&self) -> String;
    fn on_diagnostic(&self, sequence: u64);
}

#[derive(uniffi::Object, Default)]
pub struct CancellationToken {
    cancelled: AtomicBool,
}

#[uniffi::export]
impl CancellationToken {
    #[uniffi::constructor]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
}

#[derive(uniffi::Record, Debug)]
pub struct FoundationReport {
    pub ffmpeg_version: String,
    pub encoder_count: u32,
    pub decoder_count: u32,
    pub muxer_count: u32,
    pub demuxer_count: u32,
    pub filter_count: u32,
    pub storage_root: String,
    pub sequence: u64,
}

#[derive(uniffi::Object)]
pub struct Engine {
    provider: Box<dyn NativeProvider>,
    sequence: AtomicU64,
}

#[uniffi::export]
impl Engine {
    #[uniffi::constructor]
    pub fn new(provider: Box<dyn NativeProvider>) -> Arc<Self> {
        Arc::new(Self {
            provider,
            sequence: AtomicU64::new(0),
        })
    }

    /// A foundation diagnostic, not a processing request or a product capability catalog.
    /// Native callbacks execute on the calling worker thread and must be thread-safe.
    pub fn diagnostics(
        &self,
        cancellation: Arc<CancellationToken>,
    ) -> Result<FoundationReport, EngineError> {
        if cancellation.is_cancelled() {
            return Err(EngineError::Cancelled);
        }
        let storage_root = self.provider.storage_root();
        if !std::path::Path::new(&storage_root).is_absolute() {
            return Err(EngineError::InvalidStorageRoot);
        }
        let capabilities = crate::ffmpeg::capabilities();
        let sequence = self.sequence.fetch_add(1, Ordering::Relaxed) + 1;
        self.provider.on_diagnostic(sequence);
        if cancellation.is_cancelled() {
            return Err(EngineError::Cancelled);
        }
        Ok(FoundationReport {
            ffmpeg_version: capabilities.version,
            encoder_count: capabilities.encoders,
            decoder_count: capabilities.decoders,
            muxer_count: capabilities.muxers,
            demuxer_count: capabilities.demuxers,
            filter_count: capabilities.filters,
            storage_root,
            sequence,
        })
    }
}
