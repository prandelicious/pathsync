#![allow(dead_code)]

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::Path;

const COPY_BUFFER_SIZE: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CopyTransferStrategy {
    NativePreferred,
    ManualOnly,
}

#[derive(Debug)]
pub(crate) struct CopyTransferOutcome {
    pub(crate) bytes: u64,
    pub(crate) metadata: fs::Metadata,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NativeUnsupportedReason {
    NoNativeBackend,
    UnsupportedSourceKind,
}

#[derive(Debug)]
pub(crate) enum CopyTransferError {
    UnsupportedNative {
        reason: NativeUnsupportedReason,
    },
    Io {
        operation: CopyTransferOperation,
        source: io::Error,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CopyTransferOperation {
    StatSource,
    OpenSource,
    CreateDestination,
    ReadSource,
    WriteDestination,
    FlushDestination,
    NativeCopy,
}

impl CopyTransferError {
    pub(crate) fn unsupported_native(reason: NativeUnsupportedReason) -> Self {
        Self::UnsupportedNative { reason }
    }

    pub(crate) fn io(operation: CopyTransferOperation, source: io::Error) -> Self {
        Self::Io { operation, source }
    }
}

pub(crate) fn select_transfer_strategy(metadata: &fs::Metadata) -> CopyTransferStrategy {
    if metadata.file_type().is_file() {
        CopyTransferStrategy::NativePreferred
    } else {
        CopyTransferStrategy::ManualOnly
    }
}

pub(crate) fn should_fallback_to_manual(error: &CopyTransferError) -> bool {
    matches!(error, CopyTransferError::UnsupportedNative { .. })
}

pub(crate) fn copy_file_data(
    source: &Path,
    dest: &Path,
    progress: impl FnMut(u64),
) -> Result<CopyTransferOutcome, CopyTransferError> {
    let source_file = open_source_file(source)?;
    copy_file_data_from_open_source(source_file, source, dest, progress)
}

pub(crate) fn copy_file_data_from_open_source(
    source_file: File,
    source: &Path,
    dest: &Path,
    progress: impl FnMut(u64),
) -> Result<CopyTransferOutcome, CopyTransferError> {
    let metadata = source_file
        .metadata()
        .map_err(|err| CopyTransferError::io(CopyTransferOperation::StatSource, err))?;
    copy_file_data_with_strategy(
        select_transfer_strategy(&metadata),
        source_file,
        source,
        dest,
        metadata,
        progress,
    )
}

pub(crate) fn copy_file_data_with_strategy(
    strategy: CopyTransferStrategy,
    source_file: File,
    source: &Path,
    dest: &Path,
    metadata: fs::Metadata,
    mut progress: impl FnMut(u64),
) -> Result<CopyTransferOutcome, CopyTransferError> {
    match strategy {
        CopyTransferStrategy::NativePreferred => {
            match copy_file_data_native(&source_file, source, dest, &metadata, &mut progress) {
                Ok(bytes) => Ok(CopyTransferOutcome { bytes, metadata }),
                Err(error) if should_fallback_to_manual(&error) => {
                    copy_file_data_manual(source_file, source, dest, &mut progress)
                        .map(|bytes| CopyTransferOutcome { bytes, metadata })
                }
                Err(error) => Err(error),
            }
        }
        CopyTransferStrategy::ManualOnly => {
            copy_file_data_manual(source_file, source, dest, &mut progress)
                .map(|bytes| CopyTransferOutcome { bytes, metadata })
        }
    }
}

fn copy_file_data_manual(
    source_file: File,
    source: &Path,
    dest: &Path,
    progress: &mut dyn FnMut(u64),
) -> Result<u64, CopyTransferError> {
    let _source_guard = source_file;
    let source_file = open_source_file_for_manual_copy(source)
        .map_err(|err| CopyTransferError::io(CopyTransferOperation::OpenSource, err))?;
    let dest_file = open_destination_file_for_manual_copy(dest)
        .map_err(|err| CopyTransferError::io(CopyTransferOperation::CreateDestination, err))?;
    copy_file_handles_manual(source_file, dest_file, progress)
}

fn copy_file_handles_manual(
    source: File,
    dest: File,
    progress: &mut dyn FnMut(u64),
) -> Result<u64, CopyTransferError> {
    let mut reader = BufReader::with_capacity(COPY_BUFFER_SIZE, source);
    let mut writer = BufWriter::with_capacity(COPY_BUFFER_SIZE, dest);
    let mut buffer = vec![0_u8; COPY_BUFFER_SIZE];
    let mut copied = 0_u64;

    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|err| CopyTransferError::io(CopyTransferOperation::ReadSource, err))?;
        if read == 0 {
            break;
        }

        writer
            .write_all(&buffer[..read])
            .map_err(|err| CopyTransferError::io(CopyTransferOperation::WriteDestination, err))?;
        copied += read as u64;
        progress(copied);
    }

    writer
        .flush()
        .map_err(|err| CopyTransferError::io(CopyTransferOperation::FlushDestination, err))?;

    Ok(copied)
}

fn copy_file_data_native(
    source_file: &File,
    _source: &Path,
    dest: &Path,
    metadata: &fs::Metadata,
    progress: &mut dyn FnMut(u64),
) -> Result<u64, CopyTransferError> {
    if !metadata.file_type().is_file() {
        return Err(CopyTransferError::unsupported_native(
            NativeUnsupportedReason::UnsupportedSourceKind,
        ));
    }

    #[cfg(target_os = "linux")]
    {
        linux_native::copy_file_data(source_file, dest, metadata, progress)
    }

    #[cfg(target_os = "macos")]
    {
        macos_native::copy_file_data(source_file, dest, metadata, progress)
    }

    #[cfg(target_os = "windows")]
    {
        windows_native::copy_file_data(source_file, _source, dest, metadata, progress)
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        let _ = (source, dest, metadata, progress);
        Err(CopyTransferError::unsupported_native(
            NativeUnsupportedReason::NoNativeBackend,
        ))
    }
}

#[cfg(target_os = "windows")]
const FILE_FLAG_SEQUENTIAL_SCAN: u32 = 0x0800_0000;
#[cfg(target_os = "windows")]
const FILE_SHARE_READ: u32 = 0x0000_0001;

fn open_source_file(source: &Path) -> Result<File, CopyTransferError> {
    open_source_file_with_options(source)
        .map_err(|err| CopyTransferError::io(CopyTransferOperation::OpenSource, err))
}

fn open_destination_file(dest: &Path) -> Result<File, CopyTransferError> {
    open_destination_file_with_options(dest)
        .map_err(|err| CopyTransferError::io(CopyTransferOperation::CreateDestination, err))
}

fn open_source_file_for_manual_copy(source: &Path) -> io::Result<File> {
    open_file_with_options(source, false, true)
}

fn open_destination_file_for_manual_copy(dest: &Path) -> io::Result<File> {
    open_file_with_options(dest, true, true)
}

fn open_source_file_with_options(source: &Path) -> io::Result<File> {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::fs::OpenOptionsExt;

        let mut options = OpenOptions::new();
        options.read(true).share_mode(FILE_SHARE_READ);
        return options.open(source);
    }

    open_file_with_options(source, false, false)
}

fn open_destination_file_with_options(dest: &Path) -> io::Result<File> {
    open_file_with_options(dest, true, false)
}

fn open_file_with_options(path: &Path, write: bool, sequential_scan: bool) -> io::Result<File> {
    let mut options = OpenOptions::new();
    if write {
        options.write(true).create(true).truncate(true);
    } else {
        options.read(true);
    }
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(windows_source_share_mode());
        if sequential_scan {
            options.custom_flags(FILE_FLAG_SEQUENTIAL_SCAN);
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = sequential_scan;
    }
    options.open(path)
}

fn windows_source_share_mode() -> u32 {
    0x0000_0001
}

#[cfg(target_os = "windows")]
fn windows_manual_source_share_mode() -> u32 {
    windows_source_share_mode()
}

#[cfg(target_os = "windows")]
fn windows_manual_destination_share_mode() -> u32 {
    windows_source_share_mode()
}

#[cfg(target_os = "windows")]
fn windows_manual_sequential_scan_flag() -> u32 {
    FILE_FLAG_SEQUENTIAL_SCAN
}

unsafe fn erase_progress_lifetime(
    progress: &mut dyn FnMut(u64),
) -> *mut (dyn FnMut(u64) + 'static) {
    unsafe {
        std::mem::transmute::<*mut dyn FnMut(u64), *mut (dyn FnMut(u64) + 'static)>(
            progress as *mut dyn FnMut(u64),
        )
    }
}

#[cfg(target_os = "linux")]
mod linux_native {
    use super::*;
    use std::os::unix::io::AsRawFd;
    use std::ptr;

    const POSIX_FADV_SEQUENTIAL_HINT: i32 = 2;

    pub(super) fn copy_file_data(
        source_file: &File,
        dest: &Path,
        metadata: &fs::Metadata,
        progress: &mut dyn FnMut(u64),
    ) -> Result<u64, CopyTransferError> {
        let dest_file = open_destination_file(dest)?;

        let source_fd = source_file.as_raw_fd();
        let dest_fd = dest_file.as_raw_fd();
        let _ = unsafe { libc::posix_fadvise(source_fd, 0, 0, POSIX_FADV_SEQUENTIAL_HINT) };
        let _ = unsafe { libc::posix_fadvise(dest_fd, 0, 0, POSIX_FADV_SEQUENTIAL_HINT) };

        let total = metadata.len();
        let mut copied = 0_u64;

        while copied < total {
            let remaining = total - copied;
            let chunk = remaining.min(COPY_BUFFER_SIZE as u64) as usize;
            let result = unsafe {
                libc::copy_file_range(
                    source_fd,
                    ptr::null_mut(),
                    dest_fd,
                    ptr::null_mut(),
                    chunk,
                    0,
                )
            };

            if result < 0 {
                let err = io::Error::last_os_error();
                if copied == 0 && is_unsupported_errno(err.raw_os_error()) {
                    return Err(CopyTransferError::unsupported_native(
                        NativeUnsupportedReason::UnsupportedSourceKind,
                    ));
                }
                return Err(CopyTransferError::io(
                    CopyTransferOperation::NativeCopy,
                    err,
                ));
            }

            if result == 0 {
                if copied == total {
                    break;
                }

                return Err(CopyTransferError::io(
                    CopyTransferOperation::NativeCopy,
                    io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "copy_file_range returned EOF before expected size",
                    ),
                ));
            }

            copied += result as u64;
            progress(copied);
        }

        Ok(copied)
    }

    fn is_unsupported_errno(errno: Option<i32>) -> bool {
        matches!(
            errno,
            Some(code)
                if code == libc::ENOSYS
                    || code == libc::EXDEV
                    || code == libc::EINVAL
                    || code == libc::EOPNOTSUPP
        )
    }
}

#[cfg(target_os = "macos")]
mod macos_native {
    use super::*;
    use std::ffi::{c_char, c_int, c_void};
    use std::os::unix::io::AsRawFd;

    const COPYFILE_DATA_FLAG: u32 = 0x8;
    const COPYFILE_CONTINUE: c_int = 0;
    const COPYFILE_START: c_int = 1;
    const COPYFILE_FINISH: c_int = 2;
    const COPYFILE_PROGRESS: c_int = 4;
    const COPYFILE_COPY_DATA: c_int = 4;
    const COPYFILE_STATE_STATUS_CB: u32 = 6;
    const COPYFILE_STATE_STATUS_CTX: u32 = 7;
    const COPYFILE_STATE_COPIED: u32 = 8;
    const F_NOCACHE: c_int = 48;

    type CopyfileState = *mut c_void;
    type CopyfileCallback = unsafe extern "C" fn(
        c_int,
        c_int,
        CopyfileState,
        *const c_char,
        *const c_char,
        *mut c_void,
    ) -> c_int;

    struct CopyContext {
        progress: Box<dyn FnMut(u64)>,
        total: u64,
        last_reported: u64,
    }

    unsafe extern "C" {
        fn copyfile_state_alloc() -> CopyfileState;
        fn copyfile_state_free(state: CopyfileState) -> c_int;
        fn copyfile_state_set(state: CopyfileState, flag: u32, src: *const c_void) -> c_int;
        fn copyfile_state_get(state: CopyfileState, flag: u32, dst: *mut c_void) -> c_int;
        fn fcopyfile(from_fd: c_int, to_fd: c_int, state: CopyfileState, flags: u32) -> c_int;
        fn fcntl(fd: c_int, cmd: c_int, ...) -> c_int;
    }

    pub(super) fn copy_file_data(
        source_file: &File,
        dest: &Path,
        metadata: &fs::Metadata,
        progress: &mut dyn FnMut(u64),
    ) -> Result<u64, CopyTransferError> {
        let dest_file = open_destination_file(dest)?;

        let source_fd = source_file.as_raw_fd();
        let dest_fd = dest_file.as_raw_fd();
        let _ = unsafe { fcntl(source_fd, F_NOCACHE, 1) };
        let _ = unsafe { fcntl(dest_fd, F_NOCACHE, 1) };

        let state = unsafe { copyfile_state_alloc() };
        if state.is_null() {
            return Err(CopyTransferError::io(
                CopyTransferOperation::NativeCopy,
                io::Error::other("copyfile_state_alloc returned null"),
            ));
        }

        let progress_ptr = unsafe { erase_progress_lifetime(progress) };
        let mut context = Box::new(CopyContext {
            progress: Box::new(move |copied| unsafe {
                (&mut *progress_ptr)(copied);
            }),
            total: metadata.len(),
            last_reported: 0,
        });
        let context_ptr = (&mut *context as *mut CopyContext).cast::<c_void>();

        let setup_result = unsafe {
            let status_cb = copyfile_status_callback as CopyfileCallback;
            let cb_result =
                copyfile_state_set(state, COPYFILE_STATE_STATUS_CB, status_cb as *const c_void);
            if cb_result != 0 {
                Err(io::Error::last_os_error())
            } else {
                let ctx_result = copyfile_state_set(state, COPYFILE_STATE_STATUS_CTX, context_ptr);
                if ctx_result != 0 {
                    Err(io::Error::last_os_error())
                } else {
                    let result = fcopyfile(source_fd, dest_fd, state, COPYFILE_DATA_FLAG);
                    if result == 0 {
                        Ok(())
                    } else {
                        Err(io::Error::last_os_error())
                    }
                }
            }
        };

        let copied = match setup_result {
            Ok(()) => {
                if context.last_reported < context.total {
                    let progress_fn = context.progress.as_mut();
                    progress_fn(context.total);
                    context.last_reported = context.total;
                }
                context.total
            }
            Err(err) => {
                let errno = err.raw_os_error();
                if context.last_reported == 0 && is_unsupported_errno(errno) {
                    unsafe {
                        let _ = copyfile_state_free(state);
                    }
                    return Err(CopyTransferError::unsupported_native(
                        NativeUnsupportedReason::UnsupportedSourceKind,
                    ));
                }
                unsafe {
                    let _ = copyfile_state_free(state);
                }
                return Err(CopyTransferError::io(
                    CopyTransferOperation::NativeCopy,
                    err,
                ));
            }
        };

        unsafe {
            let _ = copyfile_state_free(state);
        }
        Ok(copied)
    }

    unsafe extern "C" fn copyfile_status_callback(
        what: c_int,
        stage: c_int,
        state: CopyfileState,
        _src: *const c_char,
        _dst: *const c_char,
        ctx: *mut c_void,
    ) -> c_int {
        if what == COPYFILE_COPY_DATA
            && (stage == COPYFILE_START || stage == COPYFILE_PROGRESS || stage == COPYFILE_FINISH)
        {
            unsafe {
                let context = &mut *(ctx.cast::<CopyContext>());
                let mut copied: libc::off_t = 0;
                if copyfile_state_get(
                    state,
                    COPYFILE_STATE_COPIED,
                    (&mut copied as *mut libc::off_t).cast::<c_void>(),
                ) == 0
                {
                    let copied = copied.max(0) as u64;
                    if copied > context.last_reported {
                        context.last_reported = copied;
                        let progress = context.progress.as_mut();
                        progress(copied);
                    }
                }

                if stage == COPYFILE_FINISH && context.last_reported < context.total {
                    context.last_reported = context.total;
                    let progress = context.progress.as_mut();
                    progress(context.total);
                }
            }
        }

        COPYFILE_CONTINUE
    }

    fn is_unsupported_errno(errno: Option<i32>) -> bool {
        matches!(
            errno,
            Some(code)
                if code == libc::ENOTSUP
                    || code == libc::EOPNOTSUPP
                    || code == libc::EXDEV
                    || code == libc::EINVAL
        )
    }
}

#[cfg(target_os = "windows")]
mod windows_native {
    use super::*;
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;

    const PROGRESS_CONTINUE: u32 = 0;
    const ERROR_INVALID_FUNCTION: u32 = 1;
    const ERROR_NOT_SUPPORTED: u32 = 50;
    const ERROR_INVALID_PARAMETER: u32 = 87;

    type Bool = i32;
    type Dword = u32;
    type Lpcwstr = *const u16;
    type LpprogressRoutine = Option<
        unsafe extern "system" fn(
            i64,
            i64,
            i64,
            i64,
            Dword,
            Dword,
            *mut c_void,
            *mut c_void,
            *mut c_void,
        ) -> Dword,
    >;

    struct CopyContext {
        progress: Box<dyn FnMut(u64)>,
        total: u64,
        last_reported: u64,
    }

    unsafe extern "system" {
        fn CopyFileExW(
            lpExistingFileName: Lpcwstr,
            lpNewFileName: Lpcwstr,
            lpProgressRoutine: LpprogressRoutine,
            lpData: *mut c_void,
            pbCancel: *mut i32,
            dwCopyFlags: Dword,
        ) -> Bool;
        fn GetLastError() -> Dword;
    }

    pub(super) fn copy_file_data(
        source_file: &File,
        source: &Path,
        dest: &Path,
        metadata: &fs::Metadata,
        progress: &mut dyn FnMut(u64),
    ) -> Result<u64, CopyTransferError> {
        let _source_file = source_file;
        let progress_ptr = unsafe { erase_progress_lifetime(progress) };
        let mut context = Box::new(CopyContext {
            progress: Box::new(move |copied| unsafe {
                (&mut *progress_ptr)(copied);
            }),
            total: metadata.len(),
            last_reported: 0,
        });
        let context_ptr = (&mut *context as *mut CopyContext).cast::<c_void>();

        let source_wide = wide_path(source);
        let dest_wide = wide_path(dest);
        let mut cancel = 0_i32;

        let result = unsafe {
            CopyFileExW(
                source_wide.as_ptr(),
                dest_wide.as_ptr(),
                Some(copyfile_progress_routine),
                context_ptr,
                &mut cancel,
                0,
            )
        };

        if result != 0 {
            if context.last_reported < context.total {
                let progress_fn = context.progress.as_mut();
                progress_fn(context.total);
            }
            return Ok(metadata.len());
        }

        let error = unsafe { GetLastError() };
        if context.last_reported == 0 && is_unsupported_error(error) {
            return Err(CopyTransferError::unsupported_native(
                NativeUnsupportedReason::UnsupportedSourceKind,
            ));
        }

        Err(CopyTransferError::io(
            CopyTransferOperation::NativeCopy,
            io::Error::from_raw_os_error(error as i32),
        ))
    }

    unsafe extern "system" fn copyfile_progress_routine(
        total_file_size: i64,
        total_bytes_transferred: i64,
        _stream_size: i64,
        _stream_bytes_transferred: i64,
        _dw_stream_number: Dword,
        _dw_callback_reason: Dword,
        _hsource_file: *mut c_void,
        _hdest_file: *mut c_void,
        lpdata: *mut c_void,
    ) -> Dword {
        let context = &mut *(lpdata.cast::<CopyContext>());
        let copied = total_bytes_transferred.max(0) as u64;
        let total = total_file_size.max(0) as u64;
        let target = copied.min(total.min(context.total));
        if target > context.last_reported {
            context.last_reported = target;
            let progress = &mut *context.progress;
            progress(target);
        }
        PROGRESS_CONTINUE
    }

    fn wide_path(path: &Path) -> Vec<u16> {
        let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
        wide.push(0);
        wide
    }

    fn is_unsupported_error(error: Dword) -> bool {
        matches!(
            error,
            ERROR_NOT_SUPPORTED | ERROR_INVALID_FUNCTION | ERROR_INVALID_PARAMETER
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock before unix epoch")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "pathsync-copy-fast-path-{name}-{}-{nanos}",
            std::process::id()
        ))
    }

    fn write_file(path: &Path, contents: &[u8]) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("failed to create parent directory");
        }
        let mut file = File::create(path).expect("failed to create file");
        file.write_all(contents).expect("failed to write file");
    }

    #[test]
    fn regular_files_prefer_native_strategy_and_directories_do_not() {
        let root = temp_dir("strategy");
        fs::create_dir_all(&root).expect("failed to create temp root");

        let source_file = root.join("source.txt");
        let source_dir = root.join("source-dir");
        write_file(&source_file, b"abc");
        fs::create_dir_all(&source_dir).expect("failed to create source dir");

        let file_strategy = select_transfer_strategy(
            &fs::metadata(&source_file).expect("failed to stat source file"),
        );
        let dir_strategy = select_transfer_strategy(
            &fs::metadata(&source_dir).expect("failed to stat source dir"),
        );

        assert_eq!(file_strategy, CopyTransferStrategy::NativePreferred);
        assert_eq!(dir_strategy, CopyTransferStrategy::ManualOnly);

        fs::remove_dir_all(&root).expect("failed to clean temp root");
    }

    #[test]
    fn unsupported_native_errors_are_the_only_fallback_signal() {
        let unsupported =
            CopyTransferError::unsupported_native(NativeUnsupportedReason::NoNativeBackend);
        let io_error = CopyTransferError::io(
            CopyTransferOperation::ReadSource,
            io::Error::other("disk failure"),
        );

        assert!(should_fallback_to_manual(&unsupported));
        assert!(!should_fallback_to_manual(&io_error));
    }

    #[test]
    fn windows_source_share_mode_stays_read_only() {
        assert_eq!(windows_source_share_mode(), 0x0000_0001);
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn manual_copy_helpers_request_sequential_scan() {
        assert_eq!(windows_manual_source_share_mode(), FILE_SHARE_READ);
        assert_eq!(windows_manual_destination_share_mode(), FILE_SHARE_READ);
        assert_eq!(
            windows_manual_sequential_scan_flag(),
            FILE_FLAG_SEQUENTIAL_SCAN
        );
    }

    #[test]
    fn manual_copy_reports_progress() {
        let root = temp_dir("manual-copy");
        fs::create_dir_all(&root).expect("failed to create temp root");

        let source_path = root.join("source.bin");
        let dest_path = root.join("dest.bin");
        let payload = b"manual-copy-test-bytes";
        write_file(&source_path, payload);

        let source_file = File::open(&source_path).expect("failed to open source");
        let mut progress = Vec::new();
        let outcome = copy_file_data_with_strategy(
            CopyTransferStrategy::ManualOnly,
            source_file,
            &source_path,
            &dest_path,
            fs::metadata(&source_path).expect("failed to stat source"),
            |copied| progress.push(copied),
        )
        .expect("manual copy should succeed");

        assert_eq!(outcome.bytes, payload.len() as u64);
        assert_eq!(outcome.metadata.len(), payload.len() as u64);
        assert_eq!(fs::read(&dest_path).expect("failed to read dest"), payload);
        assert_eq!(progress.last().copied(), Some(payload.len() as u64));
        assert!(progress.windows(2).all(|window| window[0] < window[1]));

        fs::remove_dir_all(&root).expect("failed to clean temp root");
    }

    #[cfg(unix)]
    #[test]
    fn open_source_handle_snapshot_is_used_for_copy_outcome() {
        let root = temp_dir("snapshot-handle");
        fs::create_dir_all(&root).expect("failed to create temp root");

        let source_path = root.join("source.bin");
        let replacement_path = root.join("replacement.bin");
        let dest_path = root.join("dest.bin");
        let original = b"original-bytes";
        let replacement = b"replacement-bytes-with-different-size";
        write_file(&source_path, original);

        let source_file = File::open(&source_path).expect("failed to open source");
        write_file(&replacement_path, replacement);
        fs::rename(&replacement_path, &source_path).expect("failed to replace source path");

        let outcome =
            copy_file_data_from_open_source(source_file, &source_path, &dest_path, |_| {})
                .expect("copy should succeed from the already-open source");

        assert_eq!(outcome.bytes, original.len() as u64);
        assert_eq!(outcome.metadata.len(), original.len() as u64);
        assert_eq!(fs::read(&dest_path).expect("failed to read dest"), original);

        fs::remove_dir_all(&root).expect("failed to clean temp root");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_native_backend_copies_and_reports_progress() {
        let root = temp_dir("macos-native");
        fs::create_dir_all(&root).expect("failed to create temp root");

        let source_path = root.join("source.bin");
        let dest_path = root.join("dest.bin");
        let payload = vec![b'x'; COPY_BUFFER_SIZE * 3 + 1024];
        write_file(&source_path, &payload);

        let source_file = File::open(&source_path).expect("failed to open source");
        let metadata = source_file.metadata().expect("failed to stat source");
        let mut progress = Vec::new();
        let copied = copy_file_data_native(
            &source_file,
            &source_path,
            &dest_path,
            &metadata,
            &mut |copied| {
                progress.push(copied);
            },
        )
        .expect("macos native copy should succeed");

        assert_eq!(copied, payload.len() as u64);
        assert_eq!(fs::read(&dest_path).expect("failed to read dest"), payload);
        assert!(progress.len() >= 2, "progress updates: {:?}", progress);
        assert!(progress[0] > 0);
        assert_eq!(progress.last().copied(), Some(payload.len() as u64));
        assert!(progress.windows(2).all(|window| window[0] < window[1]));

        fs::remove_dir_all(&root).expect("failed to clean temp root");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_native_backend_rejects_directories() {
        let root = temp_dir("macos-native-dir");
        fs::create_dir_all(&root).expect("failed to create temp root");

        let source_dir = root.join("source-dir");
        let dest_path = root.join("dest.bin");
        fs::create_dir_all(&source_dir).expect("failed to create source dir");

        let source_file = File::open(&source_dir).expect("failed to open source dir");
        let metadata = source_file.metadata().expect("failed to stat source dir");
        let mut progress = Vec::new();
        let error = copy_file_data_native(
            &source_file,
            &source_dir,
            &dest_path,
            &metadata,
            &mut |copied| {
                progress.push(copied);
            },
        )
        .expect_err("directory sources should not use native copy");

        assert!(should_fallback_to_manual(&error));
        assert!(progress.is_empty());

        fs::remove_dir_all(&root).expect("failed to clean temp root");
    }
}
