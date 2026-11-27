use crate::{
    api::{CancellationToken, EngineError, MediaProvider, OpenedSource},
    ffmpeg::sys,
};
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    ffi::{c_int, c_void},
    fs::{self, File, OpenOptions},
    io::{self, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::fs::{FileExt, MetadataExt, OpenOptionsExt},
    },
    path::{Path, PathBuf},
    ptr,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, LazyLock, Mutex,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const BUFFER_SIZE: usize = 64 * 1024;
const PROBE_TIMEOUT: Duration = Duration::from_secs(30);
const STAGING_TIMEOUT: Duration = Duration::from_secs(300);
const AVERROR_EOF: c_int = -541478725;
const AVERROR_EXIT: c_int = -1414092869;
const MAX_STREAMS: usize = 256;
static STAGE_SEQUENCE: AtomicU64 = AtomicU64::new(0);
static ACTIVE_STAGING: LazyLock<Mutex<HashSet<PathBuf>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

struct ProbeOptions(Vec<*mut sys::AVDictionary>);
impl Drop for ProbeOptions {
    fn drop(&mut self) {
        for dictionary in &mut self.0 {
            unsafe {
                sys::av_dict_free(dictionary);
            }
        }
    }
}

struct SourceLease {
    file: Option<File>,
    provider: Option<Arc<dyn MediaProvider>>,
    lease_id: String,
}

impl Drop for SourceLease {
    fn drop(&mut self) {
        // Close the transferred descriptor before allowing the native host to
        // release its document/security-scope ownership.
        drop(self.file.take());
        let lease = std::mem::take(&mut self.lease_id);
        if let Some(provider) = &self.provider {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                provider.release_source(lease);
            }));
        }
    }
}

struct Reader {
    lease: Option<SourceLease>,
    staged: Option<File>,
    staged_path: Option<PathBuf>,
    offset: u64,
    length: Option<u64>,
    position: u64,
    cancellation: Arc<CancellationToken>,
    deadline: Option<Instant>,
    io_error: Option<String>,
    io_error_is_storage: bool,
}

impl Reader {
    fn file(&self) -> &File {
        self.staged
            .as_ref()
            .unwrap_or_else(|| self.lease.as_ref().unwrap().file.as_ref().unwrap())
    }

    fn interrupted(&self) -> bool {
        self.cancellation.is_cancelled()
            || self.deadline.is_some_and(|limit| Instant::now() >= limit)
    }

    fn check(&self) -> Result<(), EngineError> {
        if self.cancellation.is_cancelled() {
            Err(EngineError::Cancelled)
        } else if self.deadline.is_some_and(|limit| Instant::now() >= limit) {
            Err(EngineError::SourceAccess {
                detail: "Reading this source exceeded the preparation time limit".into(),
            })
        } else {
            Ok(())
        }
    }

    fn read_at(&self, destination: &mut [u8], position: u64) -> io::Result<usize> {
        let count = match self.length {
            Some(length) => destination
                .len()
                .min(length.saturating_sub(position) as usize),
            None => destination.len(),
        };
        let absolute = self
            .offset
            .checked_add(position)
            .ok_or_else(|| io::Error::from_raw_os_error(libc::EOVERFLOW))?;
        if absolute > i64::MAX as u64 {
            return Err(io::Error::from_raw_os_error(libc::EOVERFLOW));
        }
        loop {
            match self.file().read_at(&mut destination[..count], absolute) {
                Err(error) if error.kind() == io::ErrorKind::Interrupted => self
                    .check()
                    .map_err(|_| io::Error::from_raw_os_error(libc::EINTR))?,
                result => return result,
            }
        }
    }
}

impl Drop for Reader {
    fn drop(&mut self) {
        drop(self.staged.take());
        drop(self.lease.take());
        if let Some(path) = self.staged_path.take() {
            // Hold the same lock as orphan reconciliation through removal. A
            // live stage remains protected until both descriptors and the native
            // source lease have been released, even during partial preparation.
            let mut active = ACTIVE_STAGING
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let _ = remove_staging_file(&path);
            active.remove(&path);
        }
    }
}

/// Owns one opened input, its AVIO callbacks and the native source lease.
/// FFmpeg only borrows Reader through a stable Box address. All access to the
/// format and its cursor stays on the worker that owns this value.
pub(crate) struct Input {
    format: *mut sys::AVFormatContext,
    io: *mut sys::AVIOContext,
    reader: Box<Reader>,
    header_duration_us: Option<i64>,
}

impl Input {
    pub(crate) fn open(
        source: OpenedSource,
        provider: Arc<dyn MediaProvider>,
        cancellation: Arc<CancellationToken>,
    ) -> Result<Self, EngineError> {
        // Ownership transfers even when validation fails; every exit must close
        // the descriptor and acknowledge release of the native lease.
        let lease = SourceLease {
            file: (source.fd >= 0).then(|| unsafe { File::from_raw_fd(source.fd) }),
            provider: Some(provider),
            lease_id: source.lease_id,
        };
        Self::open_owned(lease, source.offset, source.length, cancellation)
    }

    pub(crate) fn open_file(
        path: &std::path::Path,
        cancellation: Arc<CancellationToken>,
    ) -> Result<Self, EngineError> {
        let file = File::open(path).map_err(access_error)?;
        Self::open_owned(
            SourceLease {
                file: Some(file),
                provider: None,
                lease_id: String::new(),
            },
            0,
            None,
            cancellation,
        )
    }

    fn open_owned(
        lease: SourceLease,
        offset: u64,
        requested_length: Option<u64>,
        cancellation: Arc<CancellationToken>,
    ) -> Result<Self, EngineError> {
        if lease.file.is_none() {
            return Err(EngineError::SourceAccess {
                detail: "The source provider returned an invalid descriptor".into(),
            });
        }
        if offset > i64::MAX as u64
            || requested_length.is_some_and(|length| {
                offset
                    .checked_add(length)
                    .is_none_or(|end| end > i64::MAX as u64)
            })
        {
            return Err(EngineError::SourceAccess {
                detail: "The source descriptor range is too large".into(),
            });
        }
        let metadata = lease
            .file
            .as_ref()
            .unwrap()
            .metadata()
            .map_err(access_error)?;
        let length =
            if metadata.is_file() {
                let remaining = metadata.len().checked_sub(offset).ok_or_else(|| {
                    EngineError::SourceAccess {
                        detail: "The source offset lies beyond the file".into(),
                    }
                })?;
                if requested_length.is_some_and(|length| length > remaining) {
                    return Err(EngineError::SourceAccess {
                        detail: "The source descriptor range extends beyond the file".into(),
                    });
                }
                Some(requested_length.unwrap_or(remaining))
            } else {
                requested_length
            };
        let mut reader = Box::new(Reader {
            lease: Some(lease),
            staged: None,
            staged_path: None,
            offset,
            length,
            position: 0,
            cancellation,
            deadline: Some(Instant::now() + STAGING_TIMEOUT),
            io_error: None,
            io_error_is_storage: false,
        });
        reader.check()?;
        let fd = reader.file().as_raw_fd();
        // This checks seekability without changing the descriptor's position.
        let seekable = unsafe { libc::lseek(fd, 0, libc::SEEK_CUR) } >= 0;
        if !seekable {
            let cache = reader
                .lease
                .as_ref()
                .unwrap()
                .provider
                .as_ref()
                .ok_or_else(|| internal("A nonseekable local result cannot be inspected"))?
                .cache_root();
            stage_nonseekable(&mut reader, &PathBuf::from(cache))?;
        }
        reader.deadline = Some(Instant::now() + PROBE_TIMEOUT);
        let mut input = Self {
            format: ptr::null_mut(),
            io: ptr::null_mut(),
            reader,
            header_duration_us: None,
        };
        unsafe {
            let buffer = sys::av_malloc(BUFFER_SIZE) as *mut u8;
            if buffer.is_null() {
                return Err(internal("Unable to allocate the input buffer"));
            }
            let opaque = (&mut *input.reader as *mut Reader).cast::<c_void>();
            input.io = sys::avio_alloc_context(
                buffer,
                BUFFER_SIZE as c_int,
                0,
                opaque,
                Some(read_packet),
                None,
                Some(seek),
            );
            if input.io.is_null() {
                sys::av_free(buffer.cast());
                return Err(internal("Unable to allocate the source reader"));
            }
            (*input.io).seekable = sys::AVIO_SEEKABLE_NORMAL as c_int;
            input.format = sys::avformat_alloc_context();
            if input.format.is_null() {
                return Err(internal("Unable to allocate the media inspector"));
            }
            (*input.format).pb = input.io;
            (*input.format).io_open = Some(deny_external_open);
            (*input.format).flags |= sys::AVFMT_FLAG_CUSTOM_IO as c_int;
            (*input.format).interrupt_callback = sys::AVIOInterruptCB {
                callback: Some(interrupt),
                opaque,
            };
            (*input.format).probesize = 5 * 1024 * 1024;
            (*input.format).max_analyze_duration = 5 * i64::from(sys::AV_TIME_BASE);
            (*input.format).max_streams = MAX_STREAMS as c_int;
            let result = sys::avformat_open_input(
                &mut input.format,
                ptr::null(),
                ptr::null(),
                ptr::null_mut(),
            );
            input.check_result(result, "Unable to identify this media")?;
            // Preserve the demuxer's header value before find_stream_info can
            // synthesize an elapsed duration from stream timestamps/bit rate.
            input.header_duration_us =
                ((*input.format).duration >= 0).then_some((*input.format).duration);
            if (*input.format).nb_streams as usize > MAX_STREAMS {
                return Err(EngineError::InvalidMedia {
                    detail: "The source contains too many streams".into(),
                });
            }
            let result = {
                let mut options =
                    ProbeOptions(vec![ptr::null_mut(); (*input.format).nb_streams as usize]);
                for dictionary in &mut options.0 {
                    for (key, value) in [
                        (c"threads", c"2"),
                        (c"max_pixels", c"67108864"),
                        (c"max_samples", c"16777216"),
                    ] {
                        if sys::av_dict_set(dictionary, key.as_ptr(), value.as_ptr(), 0) < 0 {
                            return Err(internal(
                                "Unable to allocate bounded stream inspection options",
                            ));
                        }
                    }
                }
                let dictionaries = if options.0.is_empty() {
                    ptr::null_mut()
                } else {
                    options.0.as_mut_ptr()
                };
                sys::avformat_find_stream_info(input.format, dictionaries)
            };
            input.check_result(result, "Unable to inspect the media streams")?;
            if (*input.format).nb_streams == 0 || (*input.format).nb_streams as usize > MAX_STREAMS
            {
                return Err(EngineError::InvalidMedia {
                    detail: "The source does not contain a supported number of media streams"
                        .into(),
                });
            }
        }
        input.reader.check()?;
        input.reader.deadline = None;
        Ok(input)
    }

    pub(crate) fn format(&self) -> *mut sys::AVFormatContext {
        self.format
    }

    pub(crate) fn header_duration_us(&self) -> Option<i64> {
        self.header_duration_us
    }

    pub(crate) fn byte_size(&self) -> Option<u64> {
        self.reader.length
    }

    pub(crate) fn was_staged(&self) -> bool {
        self.reader.staged_path.is_some()
    }

    pub(crate) fn check_cancelled(&self) -> Result<(), EngineError> {
        self.reader.check()
    }

    pub(crate) fn check_result(&self, result: c_int, context: &str) -> Result<(), EngineError> {
        self.reader.check()?;
        if let Some(detail) = &self.reader.io_error {
            if self.reader.io_error_is_storage {
                return Err(EngineError::Storage {
                    detail: detail.clone(),
                });
            }
            return Err(EngineError::SourceAccess {
                detail: detail.clone(),
            });
        }
        if result >= 0 {
            return Ok(());
        }
        let mut text = [0 as std::ffi::c_char; 192];
        unsafe {
            sys::av_strerror(result, text.as_mut_ptr(), text.len());
        }
        let technical = unsafe { std::ffi::CStr::from_ptr(text.as_ptr()) }.to_string_lossy();
        Err(EngineError::InvalidMedia {
            detail: format!("{context}: {technical}"),
        })
    }

    /// A sampled fingerprint, not a proof that every source byte is unchanged.
    pub(crate) fn source_identity(&self) -> Result<String, EngineError> {
        self.reader.check()?;
        let original = self
            .reader
            .lease
            .as_ref()
            .unwrap()
            .file
            .as_ref()
            .unwrap()
            .metadata()
            .map_err(access_error)?;
        let mut digest = Sha256::new();
        digest.update(b"velmux-source-sampled-v1");
        if original.is_file() && self.reader.staged.is_none() {
            for value in [
                original.dev(),
                original.ino(),
                original.len(),
                original.mtime() as u64,
                original.mtime_nsec() as u64,
            ] {
                digest.update(value.to_le_bytes());
            }
        }
        // Pipe identity changes on every provider reopen. Staged sources use
        // logical range length and content samples, never a transient inode.
        digest.update(self.reader.offset.to_le_bytes());
        digest.update(self.reader.length.unwrap_or(u64::MAX).to_le_bytes());
        let mut bytes = [0u8; BUFFER_SIZE];
        let positions = match self.reader.length {
            Some(length) => vec![0, length / 2, length.saturating_sub(BUFFER_SIZE as u64)],
            None => vec![0],
        };
        for position in positions {
            self.reader.check()?;
            let count = self.reader.read_at(&mut bytes, position).map_err(|error| {
                if self.reader.staged.is_some() {
                    storage_error(error)
                } else {
                    access_error(error)
                }
            })?;
            digest.update(position.to_le_bytes());
            digest.update((count as u64).to_le_bytes());
            digest.update(&bytes[..count]);
        }
        Ok(format!("{:x}", digest.finalize()))
    }
}

impl Drop for Input {
    fn drop(&mut self) {
        unsafe {
            // Custom AVIO is not owned by avformat_close_input. FFmpeg may
            // replace its buffer, so free its current buffer rather than the
            // original allocation, then release the context itself.
            sys::avformat_close_input(&mut self.format);
            if !self.io.is_null() {
                sys::av_free((*self.io).buffer.cast());
                (*self.io).buffer = ptr::null_mut();
                sys::avio_context_free(&mut self.io);
            }
        }
    }
}

unsafe extern "C" fn deny_external_open(
    format: *mut sys::AVFormatContext,
    _io: *mut *mut sys::AVIOContext,
    _url: *const std::ffi::c_char,
    _flags: c_int,
    _options: *mut *mut sys::AVDictionary,
) -> c_int {
    if !format.is_null() {
        let opaque = (*format).interrupt_callback.opaque;
        if !opaque.is_null() {
            let reader = &mut *(opaque.cast::<Reader>());
            reader.io_error =
                Some("This media refers to another file outside the selected source".into());
            reader.io_error_is_storage = false;
        }
    }
    -libc::EACCES
}

unsafe extern "C" fn interrupt(opaque: *mut c_void) -> c_int {
    if opaque.is_null() {
        return 1;
    }
    let reader = &*(opaque.cast::<Reader>());
    c_int::from(reader.interrupted())
}

unsafe extern "C" fn read_packet(
    opaque: *mut c_void,
    destination: *mut u8,
    length: c_int,
) -> c_int {
    if opaque.is_null() || destination.is_null() || length <= 0 {
        return -libc::EINVAL;
    }
    let reader = &mut *(opaque.cast::<Reader>());
    if reader.interrupted() {
        return AVERROR_EXIT;
    }
    let output = std::slice::from_raw_parts_mut(destination, length as usize);
    match reader.read_at(output, reader.position) {
        Ok(0) => AVERROR_EOF,
        Ok(count) => {
            reader.position += count as u64;
            count as c_int
        }
        Err(error) => {
            reader.io_error_is_storage = reader.staged.is_some();
            let description = if reader.io_error_is_storage {
                "staged source cache"
            } else {
                "source"
            };
            reader.io_error = Some(format!("Unable to read the {description}: {error}"));
            -error.raw_os_error().unwrap_or(libc::EIO)
        }
    }
}

unsafe extern "C" fn seek(opaque: *mut c_void, offset: i64, whence: c_int) -> i64 {
    if opaque.is_null() {
        return -i64::from(libc::EINVAL);
    }
    let reader = &mut *(opaque.cast::<Reader>());
    if reader.interrupted() {
        return i64::from(AVERROR_EXIT);
    }
    if whence & sys::AVSEEK_SIZE as c_int != 0 {
        return reader
            .length
            .and_then(|length| i64::try_from(length).ok())
            .unwrap_or(-i64::from(libc::ENOSYS));
    }
    let base = match whence & !(sys::AVSEEK_FORCE as c_int) {
        libc::SEEK_SET => 0i128,
        libc::SEEK_CUR => i128::from(reader.position),
        libc::SEEK_END => match reader.length {
            Some(length) => i128::from(length),
            None => return -i64::from(libc::ENOSYS),
        },
        _ => return -i64::from(libc::EINVAL),
    };
    let position = base + i128::from(offset);
    if position < 0
        || position > i128::from(i64::MAX)
        || reader
            .length
            .is_some_and(|length| position > i128::from(length))
    {
        return -i64::from(libc::EINVAL);
    }
    reader.position = position as u64;
    position as i64
}

/// Reconciles only our ephemeral staging namespace. Accepted jobs retain the
/// original provider locator, never a path in this directory. After process
/// death no live input can reference these files; unknown entries are preserved.
pub(crate) fn cleanup_staging(cache: &Path) -> Result<(), EngineError> {
    if !cache.is_absolute() {
        return Err(EngineError::InvalidStorageRoot);
    }
    let cache = match cache.canonicalize() {
        Ok(cache) => cache,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(storage_error(error)),
    };
    let directory = cache.join("input-staging");
    let metadata = match fs::symlink_metadata(&directory) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(storage_error(error)),
    };
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Ok(());
    }
    let active = ACTIVE_STAGING
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    for entry in fs::read_dir(&directory).map_err(storage_error)? {
        let entry = entry.map_err(storage_error)?;
        let path = entry.path();
        if !owned_staging_name(&entry.file_name()) || active.contains(&path) {
            continue;
        }
        remove_staging_file(&path).map_err(storage_error)?;
    }
    Ok(())
}

fn owned_staging_name(name: &std::ffi::OsStr) -> bool {
    let Some(name) = name
        .to_str()
        .and_then(|name| name.strip_prefix("source-"))
        .and_then(|name| name.strip_suffix(".tmp"))
    else {
        return false;
    };
    let mut parts = name.split('-');
    (0..3).all(|_| {
        parts
            .next()
            .is_some_and(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
    }) && parts.next().is_none()
}

fn remove_staging_file(path: &Path) -> io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::from_raw_os_error(libc::EINVAL))?;
    // Do not follow a replaced staging-directory symlink, or a named file that
    // became a symlink. unlinkat stays bound to this opened directory even if
    // another thread renames its path while reconciliation is running.
    let directory = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(parent)
    {
        Ok(directory) => directory,
        Err(error)
            if matches!(
                error.raw_os_error(),
                Some(libc::ENOENT | libc::ELOOP | libc::ENOTDIR)
            ) =>
        {
            return Ok(())
        }
        Err(error) => return Err(error),
    };
    let name = std::ffi::CString::new(path.file_name().unwrap().as_bytes())
        .map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;
    let mut metadata = std::mem::MaybeUninit::<libc::stat>::uninit();
    if unsafe {
        libc::fstatat(
            directory.as_raw_fd(),
            name.as_ptr(),
            metadata.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } != 0
    {
        let error = io::Error::last_os_error();
        return if error.kind() == io::ErrorKind::NotFound {
            Ok(())
        } else {
            Err(error)
        };
    }
    if unsafe { metadata.assume_init() }.st_mode & libc::S_IFMT != libc::S_IFREG {
        return Ok(());
    }
    if unsafe { libc::unlinkat(directory.as_raw_fd(), name.as_ptr(), 0) } != 0 {
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::NotFound {
            return Err(error);
        }
    }
    Ok(())
}

fn stage_nonseekable(reader: &mut Reader, cache: &Path) -> Result<(), EngineError> {
    if !cache.is_absolute() {
        return Err(EngineError::InvalidStorageRoot);
    }
    fs::create_dir_all(cache).map_err(storage_error)?;
    let directory = cache
        .canonicalize()
        .map_err(storage_error)?
        .join("input-staging");
    fs::create_dir_all(&directory).map_err(storage_error)?;
    if fs::symlink_metadata(&directory)
        .map_err(storage_error)?
        .file_type()
        .is_symlink()
    {
        return Err(EngineError::Storage {
            detail: "The input staging directory is not a private directory".into(),
        });
    }
    let directory_handle = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(&directory)
        .map_err(storage_error)?;
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let mut staged = None;
    let mut active = ACTIVE_STAGING
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    for _ in 0..32 {
        let sequence = STAGE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = directory.join(format!(
            "source-{}-{timestamp}-{sequence}.tmp",
            std::process::id()
        ));
        let name = std::ffi::CString::new(path.file_name().unwrap().to_str().unwrap()).unwrap();
        let fd = unsafe {
            libc::openat(
                directory_handle.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDWR | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                // mode_t is u16 on Apple; C variadic arguments require its
                // integer promotion rather than passing a Rust u16 directly.
                0o600 as libc::c_int,
            )
        };
        let result = if fd < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(unsafe { File::from_raw_fd(fd) })
        };
        match result {
            Ok(file) => {
                active.insert(path.clone());
                reader.staged_path = Some(path);
                staged = Some(file);
                break;
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(storage_error(error)),
        }
    }
    drop(active);
    let mut staged = staged.ok_or_else(|| EngineError::Storage {
        detail: "Unable to reserve an input staging file".into(),
    })?;
    let fd = reader.file().as_raw_fd();
    let mut skip = reader.offset;
    let mut copied = 0u64;
    let mut buffer = [0u8; BUFFER_SIZE];
    loop {
        reader.check()?;
        if skip == 0 && reader.length.is_some_and(|length| copied >= length) {
            break;
        }
        let mut descriptor = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let ready = unsafe { libc::poll(&mut descriptor, 1, 100) };
        if ready < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(access_error(error));
        }
        if ready == 0 {
            continue;
        }
        let wanted = if skip > 0 {
            skip.min(BUFFER_SIZE as u64)
        } else {
            reader.length.map_or(BUFFER_SIZE as u64, |length| {
                length.saturating_sub(copied).min(BUFFER_SIZE as u64)
            })
        } as usize;
        let count = unsafe { libc::read(fd, buffer.as_mut_ptr().cast(), wanted) };
        if count < 0 {
            let error = io::Error::last_os_error();
            if matches!(
                error.kind(),
                io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
            ) {
                continue;
            }
            return Err(access_error(error));
        }
        if count == 0 {
            if skip > 0 || reader.length.is_some_and(|length| copied < length) {
                return Err(EngineError::SourceAccess {
                    detail: "The source ended before its declared descriptor range".into(),
                });
            }
            break;
        }
        let count = count as usize;
        if skip > 0 {
            skip -= count as u64;
            continue;
        }
        if copied % (1024 * 1024) < BUFFER_SIZE as u64 {
            let mut space = std::mem::MaybeUninit::<libc::statvfs>::uninit();
            if unsafe { libc::fstatvfs(staged.as_raw_fd(), space.as_mut_ptr()) } != 0 {
                return Err(storage_error(io::Error::last_os_error()));
            }
            let space = unsafe { space.assume_init() };
            let available = u128::from(space.f_bavail).saturating_mul(u128::from(space.f_frsize));
            if available < 8 * 1024 * 1024 + count as u128 {
                return Err(EngineError::Storage {
                    detail: "There is not enough cache space to prepare this source".into(),
                });
            }
        }
        staged.write_all(&buffer[..count]).map_err(storage_error)?;
        copied = copied
            .checked_add(count as u64)
            .filter(|length| *length <= i64::MAX as u64)
            .ok_or_else(|| EngineError::SourceAccess {
                detail: "The staged source is too large".into(),
            })?;
    }
    staged.flush().map_err(storage_error)?;
    reader.offset = 0;
    reader.length = Some(copied);
    reader.staged = Some(staged);
    Ok(())
}

fn access_error(error: io::Error) -> EngineError {
    EngineError::SourceAccess {
        detail: error.to_string(),
    }
}
fn storage_error(error: io::Error) -> EngineError {
    EngineError::Storage {
        detail: error.to_string(),
    }
}
fn internal(detail: &str) -> EngineError {
    EngineError::Internal {
        detail: detail.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Workspace(PathBuf);
    impl Workspace {
        fn new() -> Self {
            let directory =
                std::env::temp_dir().join(format!("velmux-input-test-{}", uuid::Uuid::new_v4()));
            fs::create_dir(&directory).unwrap();
            Self(directory)
        }
    }
    impl Drop for Workspace {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn reader(file: File, offset: u64, length: Option<u64>) -> Reader {
        Reader {
            lease: Some(SourceLease {
                file: Some(file),
                provider: None,
                lease_id: String::new(),
            }),
            staged: None,
            staged_path: None,
            offset,
            length,
            position: 0,
            cancellation: CancellationToken::new(),
            deadline: None,
            io_error: None,
            io_error_is_storage: false,
        }
    }

    fn wav() -> Vec<u8> {
        // Independently specified RIFF PCM: one second, 8 kHz, mono, 16 bit.
        let samples = 8000u32;
        let payload = samples * 2;
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&(36 + payload).to_le_bytes());
        bytes.extend_from_slice(b"WAVEfmt ");
        bytes.extend_from_slice(&16u32.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&8000u32.to_le_bytes());
        bytes.extend_from_slice(&16000u32.to_le_bytes());
        bytes.extend_from_slice(&2u16.to_le_bytes());
        bytes.extend_from_slice(&16u16.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&payload.to_le_bytes());
        for sample in 0..samples {
            bytes.extend_from_slice(&((sample % 251) as i16).to_le_bytes());
        }
        bytes
    }

    #[test]
    fn avio_preserves_descriptor_slice_boundaries() {
        let workspace = Workspace::new();
        let path = workspace.0.join("slice");
        fs::write(&path, b"outsideMEDIAoutside").unwrap();
        let mut state = reader(File::open(path).unwrap(), 7, Some(5));
        let pointer = (&mut state as *mut Reader).cast();
        let mut bytes = [0u8; 32];
        unsafe {
            assert_eq!(
                read_packet(pointer, bytes.as_mut_ptr(), bytes.len() as i32),
                5
            );
            assert_eq!(&bytes[..5], b"MEDIA");
            assert_eq!(
                read_packet(pointer, bytes.as_mut_ptr(), bytes.len() as i32),
                AVERROR_EOF
            );
            assert_eq!(seek(pointer, 0, sys::AVSEEK_SIZE as i32), 5);
            assert_eq!(seek(pointer, -1, libc::SEEK_END), 4);
            assert_eq!(
                read_packet(pointer, bytes.as_mut_ptr(), bytes.len() as i32),
                1
            );
            assert_eq!(bytes[0], b'A');
            assert!(seek(pointer, 6, libc::SEEK_SET) < 0);
            assert!(seek(pointer, -1, libc::SEEK_SET) < 0);
        }
    }

    #[test]
    fn actual_wav_probe_uses_offset_and_excludes_neighbor_bytes() {
        let workspace = Workspace::new();
        let path = workspace.0.join("descriptor-range");
        let payload = wav();
        let mut wrapped = b"unrelated-prefix".to_vec();
        let offset = wrapped.len() as u64;
        wrapped.extend_from_slice(&payload);
        wrapped.extend_from_slice(b"unrelated-suffix");
        fs::write(&path, wrapped).unwrap();
        let lease = SourceLease {
            file: Some(File::open(&path).unwrap()),
            provider: None,
            lease_id: String::new(),
        };
        let input = Input::open_owned(
            lease,
            offset,
            Some(payload.len() as u64),
            CancellationToken::new(),
        )
        .unwrap();
        let facts = super::super::inspect::inspect_input(&input).unwrap();
        assert_eq!(facts.container, "wav");
        assert_eq!(facts.byte_size, Some(payload.len() as u64));
        assert_eq!(facts.duration_us, Some(1_000_000));
        assert!(!facts.staged);
        assert_eq!(facts.streams.len(), 1);
        assert_eq!(facts.streams[0].codec, "pcm_s16le");
        let audio = facts.streams[0].audio.as_ref().unwrap();
        assert_eq!(audio.sample_rate, Some(8000));
        assert_eq!(audio.channels, Some(1));
        assert_eq!(audio.bit_depth, Some(16));
    }

    #[test]
    fn nonseekable_range_stages_only_its_owned_bytes_and_cleans_up() {
        let workspace = Workspace::new();
        let mut descriptors = [0; 2];
        assert_eq!(unsafe { libc::pipe(descriptors.as_mut_ptr()) }, 0);
        let read = unsafe { File::from_raw_fd(descriptors[0]) };
        let mut write = unsafe { File::from_raw_fd(descriptors[1]) };
        write.write_all(b"skipPAYLOADtail").unwrap();
        drop(write);
        let mut state = reader(read, 4, Some(7));
        stage_nonseekable(&mut state, &workspace.0).unwrap();
        assert_eq!(state.length, Some(7));
        let mut bytes = [0u8; 16];
        assert_eq!(state.read_at(&mut bytes, 0).unwrap(), 7);
        assert_eq!(&bytes[..7], b"PAYLOAD");
        let staged = state.staged_path.clone().unwrap();
        assert!(staged.is_file());
        cleanup_staging(&workspace.0).unwrap();
        assert!(staged.is_file(), "A live reader still owns this stage");
        drop(state);
        assert!(!staged.exists());
    }

    #[test]
    fn cleanup_preserves_a_pipe_stage_while_preparation_is_still_running() {
        let workspace = Workspace::new();
        let mut descriptors = [0; 2];
        assert_eq!(unsafe { libc::pipe(descriptors.as_mut_ptr()) }, 0);
        let read = unsafe { File::from_raw_fd(descriptors[0]) };
        let mut write = unsafe { File::from_raw_fd(descriptors[1]) };
        write.write_all(b"streaming input").unwrap();
        // Keep the writer open: the staging worker must remain active until
        // cancellation instead of treating a briefly empty pipe as EOF.
        let mut state = reader(read, 0, None);
        let token = state.cancellation.clone();
        let cache = workspace.0.clone();
        let worker = std::thread::spawn(move || stage_nonseekable(&mut state, &cache));
        let canonical = workspace.0.canonicalize().unwrap().join("input-staging");
        let deadline = Instant::now() + Duration::from_secs(3);
        let staged = loop {
            let found = ACTIVE_STAGING
                .lock()
                .unwrap()
                .iter()
                .find(|path| path.parent() == Some(canonical.as_path()))
                .cloned();
            if let Some(path) = found {
                break Some(path);
            }
            if Instant::now() >= deadline {
                break None;
            }
            std::thread::sleep(Duration::from_millis(5));
        };
        let result = cleanup_staging(&workspace.0);
        let preserved = staged.as_ref().is_some_and(|path| path.is_file());
        token.cancel();
        let outcome = worker.join().unwrap();
        drop(write);
        result.unwrap();
        let staged = staged.expect("The pipe worker never registered its stage");
        assert!(
            preserved,
            "Orphan cleanup removed a stage from an active pipe"
        );
        assert!(matches!(outcome, Err(EngineError::Cancelled)));
        assert!(!staged.exists());
        assert!(!ACTIVE_STAGING.lock().unwrap().contains(&staged));
    }

    #[test]
    fn cleanup_removes_owned_orphans_and_preserves_unknown_files_and_symlinks() {
        use std::os::unix::fs::symlink;
        let workspace = Workspace::new();
        let directory = workspace.0.join("input-staging");
        fs::create_dir(&directory).unwrap();
        let orphan = directory.join("source-123-456789-0.tmp");
        fs::write(&orphan, b"orphaned ephemeral bytes").unwrap();
        let unknown = [
            "source-not-a-pid-1-2.tmp",
            "source-1-2.tmp",
            "source-1-2-3-4.tmp",
            "source-1-2-3.tmp.backup",
            "source-1--3.tmp",
            "notes.txt",
        ];
        for name in unknown {
            fs::write(directory.join(name), b"preserve").unwrap();
        }
        let outside = workspace.0.join("original-media");
        fs::write(&outside, b"original bytes").unwrap();
        let linked = directory.join("source-1-2-3.tmp");
        symlink(&outside, &linked).unwrap();
        let nested = directory.join("source-9-8-7.tmp");
        fs::create_dir(&nested).unwrap();
        fs::write(nested.join("source-1-2-3.tmp"), b"not traversed").unwrap();
        cleanup_staging(&workspace.0).unwrap();
        cleanup_staging(&workspace.0).unwrap();
        assert!(!orphan.exists());
        for name in unknown {
            assert_eq!(fs::read(directory.join(name)).unwrap(), b"preserve");
        }
        assert!(fs::symlink_metadata(&linked)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(fs::read(&outside).unwrap(), b"original bytes");
        assert!(nested.join("source-1-2-3.tmp").is_file());
        let redirected = Workspace::new();
        symlink(&directory, redirected.0.join("input-staging")).unwrap();
        fs::write(&orphan, b"preserve through directory symlink").unwrap();
        cleanup_staging(&redirected.0).unwrap();
        assert!(orphan.is_file());
    }

    #[test]
    fn staging_cache_filesystem_failures_are_storage_errors() {
        let workspace = Workspace::new();
        fs::write(workspace.0.join("input-staging"), b"not a directory").unwrap();
        let path = workspace.0.join("source");
        fs::write(&path, b"source bytes").unwrap();
        let mut state = reader(File::open(path).unwrap(), 0, None);
        assert!(matches!(
            stage_nonseekable(&mut state, &workspace.0),
            Err(EngineError::Storage { .. })
        ));
    }

    #[test]
    fn stalled_pipe_preparation_acknowledges_cancellation() {
        let workspace = Workspace::new();
        let mut descriptors = [0; 2];
        assert_eq!(unsafe { libc::pipe(descriptors.as_mut_ptr()) }, 0);
        let read = unsafe { File::from_raw_fd(descriptors[0]) };
        let _write = unsafe { File::from_raw_fd(descriptors[1]) };
        let mut state = reader(read, 0, None);
        let token = state.cancellation.clone();
        let path = workspace.0.clone();
        let worker = std::thread::spawn(move || stage_nonseekable(&mut state, &path));
        token.cancel();
        assert!(matches!(
            worker.join().unwrap(),
            Err(EngineError::Cancelled)
        ));
        assert_eq!(
            fs::read_dir(workspace.0.join("input-staging"))
                .map(|entries| entries.count())
                .unwrap_or(0),
            0
        );
    }

    #[test]
    fn invalid_probe_and_precancelled_probe_return_typed_errors() {
        let workspace = Workspace::new();
        let path = workspace.0.join("invalid");
        fs::write(&path, b"this is not media").unwrap();
        assert!(matches!(
            Input::open_file(&path, CancellationToken::new()),
            Err(EngineError::InvalidMedia { .. })
        ));
        let token = CancellationToken::new();
        token.cancel();
        assert!(matches!(
            Input::open_file(&path, token),
            Err(EngineError::Cancelled)
        ));
    }
}
