use super::EngineError;
use serde::{Deserialize, Serialize};

/// The descriptor is transferred to Rust. Native security scope outlives the
/// descriptor and is released only by release_source after Rust closes it.
#[derive(Debug, uniffi::Record)]
pub struct OpenedSource {
    pub fd: i32,
    pub offset: u64,
    pub length: Option<u64>,
    pub lease_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, uniffi::Record)]
pub struct OutputReservation {
    pub locator: String,
    pub display_name: String,
    pub mime_type: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, uniffi::Record)]
pub struct PublishedOutput {
    pub locator: String,
    pub display_name: String,
    pub size: u64,
}

#[uniffi::export(callback_interface)]
pub trait MediaProvider: Send + Sync {
    fn cache_root(&self) -> String;
    fn output_location(&self) -> String;
    /// Acquire durable OS access for a locator before its first Rust use. This
    /// is separate from the lifetime of any individual transferred descriptor.
    fn retain_source_access(&self, locator: String) -> Result<(), EngineError>;
    /// Best-effort release of only this application's acquired durable access.
    /// Never close an existing descriptor lease or modify the original document.
    fn release_source_access(&self, locator: String);
    /// At startup, retire only owned OS grant receipts not referenced by these
    /// durable requests. Unknown grants and original documents stay untouched.
    fn reconcile_source_access(&self, retained_locators: Vec<String>);
    fn open_source(&self, locator: String) -> Result<OpenedSource, EngineError>;
    fn release_source(&self, lease_id: String);
    /// Reserve an unpublished destination. Repeated calls for a job must return
    /// its same reservation, without opening or overwriting unrelated files.
    fn reserve_output(
        &self,
        job_id: String,
        file_name: String,
        mime_type: String,
    ) -> Result<OutputReservation, EngineError>;
    /// Make a fully verified working file accessible atomically where supported.
    /// Repeating publication of the same reservation is idempotent.
    fn publish_output(
        &self,
        reservation: OutputReservation,
        working_path: String,
        expected_size: u64,
    ) -> Result<PublishedOutput, EngineError>;
    fn publication_status(
        &self,
        reservation: OutputReservation,
    ) -> Result<Option<PublishedOutput>, EngineError>;
    /// Remove only a still-unpublished reservation, never a published user file.
    fn discard_reservation(&self, reservation: OutputReservation);
}
