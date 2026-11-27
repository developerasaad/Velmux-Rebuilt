use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc, Mutex,
};

mod platform;
pub use platform::*;

#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum EngineError {
    #[error("The request was canceled")]
    Cancelled,
    #[error("The native host did not supply an absolute private storage directory")]
    InvalidStorageRoot,
    #[error("Source access: {detail}")]
    SourceAccess { detail: String },
    #[error("Unsupported media: {detail}")]
    UnsupportedMedia { detail: String },
    #[error("Invalid media: {detail}")]
    InvalidMedia { detail: String },
    #[error("Configuration: {detail}")]
    Configuration { detail: String },
    #[error("Decoding: {detail}")]
    Decoding { detail: String },
    #[error("Encoding: {detail}")]
    Encoding { detail: String },
    #[error("Filtering: {detail}")]
    Filtering { detail: String },
    #[error("Muxing: {detail}")]
    Muxing { detail: String },
    #[error("Storage: {detail}")]
    Storage { detail: String },
    #[error("Recovery: {detail}")]
    Recovery { detail: String },
    #[error("Platform: {detail}")]
    Platform { detail: String },
    #[error("Internal failure: {detail}")]
    Internal { detail: String },
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
    runtime: Mutex<Option<Arc<crate::jobs::Runtime>>>,
}

#[uniffi::export]
impl Engine {
    #[uniffi::constructor]
    pub fn new(provider: Box<dyn NativeProvider>) -> Arc<Self> {
        Arc::new(Self {
            provider,
            sequence: AtomicU64::new(0),
            runtime: Mutex::new(None),
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

    pub fn initialize_media(&self, provider: Box<dyn MediaProvider>) -> Result<(), EngineError> {
        let mut runtime = self.runtime.lock().map_err(|e| EngineError::Internal {
            detail: e.to_string(),
        })?;
        if runtime.is_some() {
            return Err(EngineError::Internal {
                detail: "The media runtime is already initialized.".into(),
            });
        }
        *runtime = Some(crate::jobs::Runtime::open(
            self.provider.storage_root(),
            provider,
        )?);
        Ok(())
    }

    pub fn observe_jobs(&self, observer: Box<dyn crate::JobObserver>) -> Result<(), EngineError> {
        self.media_runtime()?.observe(observer);
        Ok(())
    }
    pub fn inspect_source(
        &self,
        locator: String,
        display_name: String,
        operation: crate::Operation,
        cancellation: Arc<CancellationToken>,
    ) -> Result<crate::InspectedSource, EngineError> {
        self.media_runtime()?
            .inspect(locator, display_name, operation, cancellation)
    }
    pub fn release_inspection(&self, id: String) -> Result<(), EngineError> {
        self.media_runtime()?.release(&id);
        Ok(())
    }
    pub fn default_configuration(
        &self,
        id: String,
        operation: crate::Operation,
    ) -> Result<crate::ConversionConfig, EngineError> {
        self.media_runtime()?.defaults(&id, operation)
    }
    pub fn media_catalog(&self, operation: crate::Operation) -> Vec<crate::FormatOption> {
        crate::configuration::catalog(operation)
    }
    pub fn media_catalog_for_source(
        &self,
        id: String,
        operation: crate::Operation,
    ) -> Result<Vec<crate::FormatOption>, EngineError> {
        self.media_runtime()?.source_catalog(&id, operation)
    }
    pub fn validate_configuration(
        &self,
        id: String,
        configuration: crate::ConversionConfig,
    ) -> Result<(), EngineError> {
        self.media_runtime()?.assess(&id, &configuration)
    }
    pub fn submit_conversion(
        &self,
        id: String,
        submission_key: String,
        configuration: crate::ConversionConfig,
        cancellation: Arc<CancellationToken>,
    ) -> Result<crate::JobRecord, EngineError> {
        self.media_runtime()?
            .submit(&id, submission_key, configuration, cancellation)
    }
    pub fn jobs(&self) -> Result<Vec<crate::JobRecord>, EngineError> {
        self.media_runtime()?.jobs()
    }
    pub fn runtime_error(&self) -> Result<Option<String>, EngineError> {
        Ok(self.media_runtime()?.runtime_error())
    }
    pub fn cancel_job(&self, id: String) -> Result<(), EngineError> {
        self.media_runtime()?.cancel(&id)
    }
    pub fn restart_job(&self, id: String) -> Result<(), EngineError> {
        self.media_runtime()?.restart(&id)
    }
    pub fn retry_publication(&self, id: String) -> Result<(), EngineError> {
        self.media_runtime()?.retry_publication(&id)
    }
    pub fn dismiss_job(&self, id: String) -> Result<(), EngineError> {
        self.media_runtime()?.dismiss(&id)
    }
    pub fn appearance(&self) -> Result<String, EngineError> {
        self.media_runtime()?.appearance()
    }
    pub fn set_appearance(&self, value: String) -> Result<(), EngineError> {
        self.media_runtime()?.set_appearance(&value)
    }
    pub fn output_location(&self) -> Result<String, EngineError> {
        Ok(self.media_runtime()?.provider.output_location())
    }
    pub fn prepare_preview(
        &self,
        id: String,
        start_us: i64,
        cancellation: Arc<CancellationToken>,
    ) -> Result<crate::PreviewResult, EngineError> {
        self.media_runtime()?.preview(&id, start_us, cancellation)
    }
    pub fn cache_bytes(&self) -> Result<u64, EngineError> {
        self.media_runtime()?.cache_bytes()
    }
    pub fn clear_cache(&self) -> Result<(), EngineError> {
        self.media_runtime()?.clear_cache()
    }
}

impl Engine {
    fn media_runtime(&self) -> Result<Arc<crate::jobs::Runtime>, EngineError> {
        self.runtime
            .lock()
            .map_err(|e| EngineError::Internal {
                detail: e.to_string(),
            })?
            .clone()
            .ok_or_else(|| EngineError::Internal {
                detail: "The media runtime has not been initialized.".into(),
            })
    }
}
impl Drop for Engine {
    fn drop(&mut self) {
        if let Ok(runtime) = self.runtime.get_mut() {
            if let Some(runtime) = runtime.take() {
                runtime.shutdown();
            }
        }
    }
}
