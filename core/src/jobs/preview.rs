use super::*;
use std::collections::HashSet;

#[derive(Clone, Debug, uniffi::Record)]
pub struct PreviewResult {
    pub locator: String,
    pub start_us: i64,
    pub duration_us: i64,
}
pub(super) struct PreviewWork {
    source_id: String,
    token: Arc<CancellationToken>,
    path: PathBuf,
    ready: bool,
}
impl PreviewWork {
    pub(super) fn cancel(&self) {
        self.token.cancel();
    }
}

impl Runtime {
    fn preview_directory(&self) -> Result<PathBuf, EngineError> {
        let cache = PathBuf::from(self.provider.cache_root());
        if !cache.is_absolute() {
            return Err(EngineError::InvalidStorageRoot);
        }
        fs::create_dir_all(&cache).map_err(storage)?;
        let directory = cache.canonicalize().map_err(storage)?.join("previews");
        fs::create_dir_all(&directory).map_err(storage)?;
        if fs::symlink_metadata(&directory)
            .map_err(storage)?
            .file_type()
            .is_symlink()
        {
            return Err(storage(
                "The preview directory is not an owned cache directory.",
            ));
        }
        Ok(directory)
    }
    pub fn preview(
        &self,
        id: &str,
        start_us: i64,
        token: Arc<CancellationToken>,
    ) -> Result<PreviewResult, EngineError> {
        let source = self.asset(id)?;
        let plan = processing::preview_plan(&source.info, start_us)?;
        let range = plan
            .preview_range
            .as_ref()
            .ok_or_else(|| internal("Missing preview range"))?
            .clone();
        let generation = uuid::Uuid::new_v4().to_string();
        let path = self
            .preview_directory()?
            .join(format!("{generation}.{}", plan.output()?.extension));
        {
            let mut previews = self.previews.lock().map_err(internal)?;
            previews.retain(|_, old| {
                if old.source_id != id {
                    return true;
                }
                old.token.cancel();
                if old.ready {
                    let _ = fs::remove_file(&old.path);
                    false
                } else {
                    true
                }
            });
            previews.insert(
                generation.clone(),
                PreviewWork {
                    source_id: id.into(),
                    token: token.clone(),
                    path: path.clone(),
                    ready: false,
                },
            );
        }
        let result = (|| {
            // One bounded preview conversion may accompany the job worker.
            // Obsolete queued requests acquire no descriptors or frame buffers.
            let _slot = self.preview_slot.lock().map_err(internal)?;
            if token.is_cancelled() {
                return Err(EngineError::Cancelled);
            }
            let opened = self.provider.open_source(source.locator.clone())?;
            let input = Input::open(opened, self.provider.clone(), token.clone())?;
            if input.source_identity()? != source.info.source_identity {
                return Err(EngineError::SourceAccess {
                    detail: "The source changed before preview preparation. Select it again."
                        .into(),
                });
            }
            processing::convert(input, &source.info, &plan, &path, token.clone(), |_| Ok(()))?;
            let output = media::inspect_file(&path, token.clone())?;
            if output.byte_size == Some(0) || output.streams.is_empty() {
                return Err(EngineError::InvalidMedia {
                    detail: "The preview contains no playable media.".into(),
                });
            }
            processing::verify_output(&output, &source.info, &plan)?;
            output
                .duration_us
                .filter(|duration| *duration > 0)
                .ok_or_else(|| EngineError::InvalidMedia {
                    detail: "The finalized preview duration is unknown.".into(),
                })
        })();
        let mut previews = self.previews.lock().map_err(internal)?;
        let current = previews.contains_key(&generation);
        if result.is_err()
            || token.is_cancelled()
            || !current
            || !self.assets.lock().map_err(internal)?.contains_key(id)
        {
            if current {
                previews.remove(&generation);
            }
            drop(previews);
            let _ = fs::remove_file(&path);
            result?;
            return Err(EngineError::Cancelled);
        }
        let actual_duration = result?;
        previews.get_mut(&generation).unwrap().ready = true;
        Ok(PreviewResult {
            locator: format!("file://{}", path.to_string_lossy()),
            start_us: range.start_us,
            duration_us: actual_duration,
        })
    }
    pub(crate) fn release_preview(&self, id: &str) {
        if let Ok(mut previews) = self.previews.lock() {
            previews.retain(|_, entry| {
                if entry.source_id != id {
                    return true;
                }
                entry.token.cancel();
                if entry.ready {
                    let _ = fs::remove_file(&entry.path);
                    false
                } else {
                    true
                }
            });
        }
    }
    pub fn cache_bytes(&self) -> Result<u64, EngineError> {
        let root = PathBuf::from(self.provider.cache_root());
        let mut total = 0u64;
        for name in ["previews", "input-staging"] {
            let directory = root.join(name);
            if !directory.exists() {
                continue;
            }
            for entry in fs::read_dir(directory).map_err(storage)?.take(10000) {
                let entry = entry.map_err(storage)?;
                let meta = entry.path().symlink_metadata().map_err(storage)?;
                if meta.is_file() {
                    total = total.saturating_add(meta.len());
                }
            }
        }
        Ok(total)
    }
    pub fn clear_cache(&self) -> Result<(), EngineError> {
        crate::media::input::cleanup_staging(&PathBuf::from(self.provider.cache_root()))?;
        let previews = self.previews.lock().map_err(internal)?;
        let referenced: HashSet<_> = previews.values().map(|entry| entry.path.clone()).collect();
        for entry in fs::read_dir(self.preview_directory()?)
            .map_err(storage)?
            .take(10000)
        {
            let path = entry.map_err(storage)?.path();
            let owned = path
                .file_stem()
                .and_then(|value| value.to_str())
                .is_some_and(|value| uuid::Uuid::parse_str(value).is_ok());
            if owned
                && !referenced.contains(&path)
                && path.symlink_metadata().map_err(storage)?.is_file()
            {
                fs::remove_file(path).map_err(storage)?;
            }
        }
        Ok(())
    }
}
