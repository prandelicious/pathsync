//! Spool store: run-scoped spool directory, capacity accounting with
//! blocking reservation, refcounted entries, eviction, and orphan cleanup.
//!
//! This module is intentionally UI-free: it knows nothing about progress
//! events or rendering and is fully unit-testable with temp directories.

use std::collections::{HashMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::error::SpoolError;

const LOCK_FILE_NAME: &str = ".pathsync-lock";
/// Bounded wait so a blocked reservation re-polls the min-free-space guard
/// even when no eviction happens (e.g. external disk activity frees space).
const RESERVE_WAIT_TIMEOUT: Duration = Duration::from_secs(1);

/// Opaque handle to a registered spool entry, returned by [`SpoolStore::register`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct EntryId(u64);

struct SpoolEntry {
    path: PathBuf,
    size: u64,
    /// Target indices that still need to read this entry. Removed one at a
    /// time by `mark_terminal`; the entry evicts when this becomes empty.
    remaining_targets: HashSet<usize>,
}

struct Inner {
    used: u64,
    entries: HashMap<u64, SpoolEntry>,
    next_entry_id: u64,
    /// Count of entries still registered (not yet evicted). Used by the
    /// deadlock-prevention check: if this is zero, nothing could possibly be
    /// evicted to free capacity, so a blocked reservation must fail fast
    /// instead of waiting forever.
    live_entry_count: usize,
    /// Count of capacity claims granted by [`reserve`](SpoolStore::reserve)
    /// that have not yet been resolved by a matching
    /// [`register`](SpoolStore::register) call. A claim is "in flight"
    /// during the real, non-trivial window between a caller's successful
    /// `reserve()` and the later `register()` it makes only after a full
    /// file copy -- so `live_entry_count` alone (which only counts
    /// registered, evictable entries) undercounts what could still free
    /// capacity. Every `reserve()` grant increments this; every `register()`
    /// call decrements it exactly once (register() is always the one path,
    /// including panic-safety cleanup in stage.rs, that eventually resolves
    /// a claim). See the deadlock-prevention check in `reserve()`.
    outstanding_claims: usize,
}

/// Owns a run-scoped spool directory on internal storage: capacity
/// accounting with blocking reservation, refcounted entries, eviction, and
/// orphan cleanup of stale sibling run directories from crashed runs.
pub(crate) struct SpoolStore {
    run_dir: PathBuf,
    spool_root: PathBuf,
    /// Path to this run's job-scoped lock file (see
    /// [`acquire_job_lock`](SpoolStore::acquire_job_lock)), removed on
    /// [`close`](SpoolStore::close)/[`Drop`] alongside `run_dir`.
    job_lock_path: PathBuf,
    cap: Option<u64>,
    min_free_bytes: u64,
    state: Mutex<Inner>,
    cond: Condvar,
    closed: AtomicBool,
}

impl SpoolStore {
    /// Opens (creating if needed) the spool store rooted at `spool_root` for
    /// `job_name`.
    ///
    /// On construction:
    /// - Atomically claims this job's lock file
    ///   ([`acquire_job_lock`](Self::acquire_job_lock)) via exclusive
    ///   filesystem creation (`O_EXCL`), so two processes racing to start
    ///   the same job can never both conclude "safe to proceed": the
    ///   filesystem itself picks exactly one winner. Fails with
    ///   [`SpoolError::ConcurrentRunDetected`] if another live process
    ///   already holds it.
    /// - Scans `spool_root` for sibling run directories belonging to the
    ///   same job (`<job_name>-` prefix). If any sibling names a live pid in
    ///   its lockfile, construction fails with
    ///   [`SpoolError::ConcurrentRunDetected`] and nothing is touched or
    ///   deleted (a two-pass scan: detect-then-clean, so a live conflict
    ///   never leaves partial cleanup behind).
    /// - Otherwise, siblings whose lockfile is absent or names a dead pid
    ///   are removed recursively as orphans from a crashed prior run.
    /// - Creates this run's own directory and writes its pid-bearing
    ///   lockfile.
    ///
    /// `max_bytes` is the capacity cap (`None` = unbounded); `min_free_bytes`
    /// is the minimum free space that must remain on the spool volume after
    /// a reservation is granted.
    pub(crate) fn open(
        spool_root: &Path,
        job_name: &str,
        max_bytes: Option<u64>,
        min_free_bytes: u64,
    ) -> Result<Self, SpoolError> {
        fs::create_dir_all(spool_root).map_err(|e| SpoolError::Io {
            op: "create spool root",
            path: spool_root.to_path_buf(),
            source: e,
        })?;

        let job_lock_path = Self::acquire_job_lock(spool_root, job_name)?;

        // From here on, any error must release the job lock we just
        // claimed: no `SpoolStore` will exist yet to release it via `Drop`.
        match Self::open_after_job_lock(spool_root, job_name) {
            Ok((run_dir, state)) => Ok(Self {
                run_dir,
                spool_root: spool_root.to_path_buf(),
                job_lock_path,
                cap: max_bytes,
                min_free_bytes,
                state: Mutex::new(state),
                cond: Condvar::new(),
                closed: AtomicBool::new(false),
            }),
            Err(e) => {
                let _ = fs::remove_file(&job_lock_path);
                Err(e)
            }
        }
    }

    /// Runs the (pre-existing) orphan-cleanup/live-lock scan and creates
    /// this run's own directory and lockfile. Split out from `open` purely
    /// so `open` can uniformly release the job lock on any error from this
    /// point onward.
    fn open_after_job_lock(
        spool_root: &Path,
        job_name: &str,
    ) -> Result<(PathBuf, Inner), SpoolError> {
        Self::clean_orphans_and_check_live_lock(spool_root, job_name)?;

        let run_id = generate_run_id();
        let run_dir = spool_root.join(format!("{job_name}-{run_id}"));
        fs::create_dir_all(&run_dir).map_err(|e| SpoolError::Io {
            op: "create run directory",
            path: run_dir.clone(),
            source: e,
        })?;
        let lock_path = run_dir.join(LOCK_FILE_NAME);
        // Second line embeds the job name explicitly, so sibling scans can
        // confirm job identity by exact match against lockfile content
        // instead of a directory-name prefix match (which would otherwise
        // let e.g. job `backup`'s prefix `backup-` swallow job
        // `backup-nightly`'s directories). See `sibling_belongs_to_job`.
        fs::write(&lock_path, format!("{}\n{job_name}\n", std::process::id())).map_err(|e| {
            SpoolError::Io {
                op: "write lockfile",
                path: lock_path,
                source: e,
            }
        })?;

        Ok((
            run_dir,
            Inner {
                used: 0,
                entries: HashMap::new(),
                next_entry_id: 0,
                live_entry_count: 0,
                outstanding_claims: 0,
            },
        ))
    }

    /// Path to this job's atomic claim file: a single, job-scoped (not
    /// run-scoped) fixed path shared by every `open()` call for the same
    /// `job_name`, distinct from the per-run `.pathsync-lock` files inside
    /// each `<job_name>-<run_id>` directory.
    fn job_lock_path(spool_root: &Path, job_name: &str) -> PathBuf {
        spool_root.join(format!(".{job_name}.pathsync-lock"))
    }

    /// Atomically claims this job's lock file via exclusive creation
    /// (`create_new`, i.e. `O_EXCL` on Unix), which the filesystem
    /// guarantees succeeds for at most one caller when raced by multiple
    /// processes -- closing the TOCTOU window a plain read-then-write
    /// sequence would leave open. Returns the claimed lock file's path,
    /// which the caller owns for the lifetime of the resulting
    /// [`SpoolStore`] and must remove on error/close/drop.
    ///
    /// If the lock file already exists, reads the pid inside: a live pid
    /// means another process genuinely holds this job's lock, so this fails
    /// with [`SpoolError::ConcurrentRunDetected`]. A dead or genuinely
    /// unreadable pid means the file is a stale leftover from a crashed
    /// prior run; it is removed and the claim is retried.
    ///
    /// Reading the pid retries briefly (bounded, short sleeps) on empty or
    /// unparseable content before concluding "stale": `create_new` and the
    /// content write below are two separate syscalls, not one atomic unit,
    /// so a racing reader can otherwise observe the file mid-creation
    /// (0 bytes) and wrongly treat a live, just-claimed lock as stale.
    fn acquire_job_lock(spool_root: &Path, job_name: &str) -> Result<PathBuf, SpoolError> {
        let lock_path = Self::job_lock_path(spool_root, job_name);
        loop {
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&lock_path)
            {
                Ok(mut file) => {
                    // Best-effort: if the pid write fails, we still hold the
                    // exclusive claim (the file exists and we created it),
                    // which is what actually matters for correctness. A
                    // missing/unreadable pid on a live claim is treated as
                    // "alive" by a subsequent racer's `is_pid_alive`
                    // ambiguous-error fallback, so this fails safe.
                    let _ = file.write_all(std::process::id().to_string().as_bytes());
                    return Ok(lock_path);
                }
                Err(e) if e.kind() == ErrorKind::AlreadyExists => {
                    match Self::read_lock_pid_with_retry(&lock_path) {
                        Some(pid) if is_pid_alive(pid) => {
                            return Err(SpoolError::ConcurrentRunDetected {
                                job_name: job_name.to_string(),
                                pid,
                                path: lock_path,
                            });
                        }
                        // Dead pid, or still unreadable/unparseable after
                        // retrying: stale leftover from a crashed run.
                        // Remove and retry the exclusive create; if another
                        // process wins the retry, the next iteration's
                        // alive-pid check catches it.
                        _ => {
                            let _ = fs::remove_file(&lock_path);
                        }
                    }
                }
                Err(e) => {
                    return Err(SpoolError::Io {
                        op: "create job lockfile",
                        path: lock_path,
                        source: e,
                    });
                }
            }
        }
    }

    /// Reads and parses the pid from `lock_path`, retrying briefly (bounded
    /// attempts, short sleeps) if the content is missing/empty/unparseable
    /// before giving up. See [`acquire_job_lock`](Self::acquire_job_lock)
    /// for why this retry exists: closing the window where a lockfile has
    /// been `create_new`'d but its content write hasn't landed yet.
    fn read_lock_pid_with_retry(lock_path: &Path) -> Option<i32> {
        const ATTEMPTS: u32 = 20;
        const RETRY_DELAY: Duration = Duration::from_millis(2);
        for attempt in 0..ATTEMPTS {
            if let Ok(contents) = fs::read_to_string(lock_path)
                && let Some(pid) = sibling_pid(&contents)
            {
                return Some(pid);
            }
            if attempt + 1 < ATTEMPTS {
                std::thread::sleep(RETRY_DELAY);
            }
        }
        None
    }

    /// Two-pass sibling scan: first detect whether any same-job sibling
    /// holds a live lock (failing fast, touching nothing, if so), then
    /// remove orphaned siblings (absent or dead-pid lockfile).
    ///
    /// Job identity is decided by EXACT match, never a directory-name
    /// prefix match: a raw `starts_with("{job_name}-")` would let a shorter
    /// job name (e.g. `backup`) swallow a longer sibling job's directories
    /// (e.g. `backup-nightly-<run_id>`), one-directionally. See
    /// `sibling_belongs_to_job`.
    fn clean_orphans_and_check_live_lock(
        spool_root: &Path,
        job_name: &str,
    ) -> Result<(), SpoolError> {
        let mut orphans: Vec<PathBuf> = Vec::new();

        for entry in fs::read_dir(spool_root).map_err(|e| SpoolError::Io {
            op: "read spool root",
            path: spool_root.to_path_buf(),
            source: e,
        })? {
            let entry = entry.map_err(|e| SpoolError::Io {
                op: "read spool root entry",
                path: spool_root.to_path_buf(),
                source: e,
            })?;
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let name = entry.file_name();
            let name = name.to_string_lossy();

            let lock_path = path.join(LOCK_FILE_NAME);
            let lock_contents = fs::read_to_string(&lock_path).ok();
            if !sibling_belongs_to_job(&name, lock_contents.as_deref(), job_name) {
                continue;
            }

            match lock_contents.as_deref().and_then(sibling_pid) {
                Some(pid) if is_pid_alive(pid) => {
                    return Err(SpoolError::ConcurrentRunDetected {
                        job_name: job_name.to_string(),
                        pid,
                        path,
                    });
                }
                _ => orphans.push(path),
            }
        }

        for path in orphans {
            let _ = fs::remove_dir_all(&path);
        }

        Ok(())
    }

    /// The run-scoped spool directory for this store.
    pub(crate) fn run_dir(&self) -> &Path {
        &self.run_dir
    }

    /// Currently reserved/used bytes in the spool, for observability only
    /// (e.g. sampling peak spool usage for the post-run summary). Purely
    /// read-only: does not participate in capacity or reservation logic.
    pub(crate) fn used_bytes(&self) -> u64 {
        self.state.lock().unwrap().used
    }

    /// Blocks the calling thread until `bytes` can be reserved: the
    /// capacity cap (if any) is not exceeded and the spool volume's
    /// available free space stays at or above the configured min-free
    /// guard after the reservation. On success, `bytes` is added to the
    /// used total.
    ///
    /// Fails immediately (without blocking) rather than waiting when:
    /// - `bytes` alone exceeds the entire cap
    ///   ([`SpoolError::ExceedsCapacity`]); or
    /// - the reservation cannot currently be satisfied, there are zero live
    ///   (unevicted) entries in the store, AND there are zero outstanding
    ///   claims (reservations granted but not yet resolved by a matching
    ///   `register()` call) -- meaning nothing registered and nothing
    ///   in-flight could possibly free capacity
    ///   ([`SpoolError::WouldDeadlock`]). A claim still in flight might
    ///   resolve into a registered, evictable entry, so it alone is enough
    ///   to keep waiting rather than fail fast.
    pub(crate) fn reserve(&self, bytes: u64) -> Result<(), SpoolError> {
        if let Some(cap) = self.cap
            && bytes > cap
        {
            return Err(SpoolError::ExceedsCapacity {
                requested: bytes,
                cap,
            });
        }

        let mut guard = self.state.lock().unwrap();
        loop {
            let cap_ok = self.cap.map(|c| guard.used + bytes <= c).unwrap_or(true);
            let free = fs4::available_space(&self.spool_root).map_err(|e| SpoolError::Io {
                op: "query available space",
                path: self.spool_root.clone(),
                source: e,
            })?;
            let min_free_ok = free
                .checked_sub(bytes)
                .is_some_and(|remaining| remaining >= self.min_free_bytes);

            if cap_ok && min_free_ok {
                guard.used += bytes;
                guard.outstanding_claims += 1;
                return Ok(());
            }

            if guard.live_entry_count == 0 && guard.outstanding_claims == 0 {
                return Err(SpoolError::WouldDeadlock { requested: bytes });
            }

            let (next_guard, _) = self.cond.wait_timeout(guard, RESERVE_WAIT_TIMEOUT).unwrap();
            guard = next_guard;
        }
    }

    /// Registers a new spool entry after a successful [`reserve`](Self::reserve).
    ///
    /// `pending_targets` is the set of target-lane indices that still need
    /// to read this entry before it can be evicted (not merely a count: the
    /// explicit indices let [`mark_terminal`](Self::mark_terminal) and
    /// [`mark_all_remaining_terminal_for_target`](Self::mark_all_remaining_terminal_for_target)
    /// be idempotent per (entry, target) pair and let the bulk hook release
    /// only the entries that actually still need the given target).
    ///
    /// If `pending_targets` is empty, the entry evicts immediately (the
    /// spool file at `path` is deleted and its bytes released).
    pub(crate) fn register(
        &self,
        path: PathBuf,
        size: u64,
        pending_targets: impl IntoIterator<Item = usize>,
    ) -> EntryId {
        let mut guard = self.state.lock().unwrap();
        let id = guard.next_entry_id;
        guard.next_entry_id += 1;
        // This register() call resolves the claim `reserve()` granted for
        // this entry's bytes: it converts an in-flight, unregistered claim
        // into a tracked entry. `saturating_sub` tolerates test-only helpers
        // that call `register()` without a preceding `reserve()`.
        guard.outstanding_claims = guard.outstanding_claims.saturating_sub(1);

        let remaining: HashSet<usize> = pending_targets.into_iter().collect();
        let already_empty = remaining.is_empty();
        guard.entries.insert(
            id,
            SpoolEntry {
                path,
                size,
                remaining_targets: remaining,
            },
        );
        guard.live_entry_count += 1;

        if already_empty {
            evict_locked(&mut guard, id);
            self.cond.notify_all();
        }

        EntryId(id)
    }

    /// Marks `entry_id` terminal (verified or failed — both are terminal)
    /// for `target_index`. When every target that needed the entry has
    /// reached a terminal outcome, the spool file is deleted, its reserved
    /// bytes are released, and blocked [`reserve`](Self::reserve) callers
    /// are woken.
    ///
    /// **Idempotency**: calling this a second time for the same (entry,
    /// target) pair is a documented no-op, not a panic or double-decrement.
    /// This matters because a panicking lane's drop guard
    /// ([`mark_all_remaining_terminal_for_target`](Self::mark_all_remaining_terminal_for_target))
    /// may race with or follow a normal in-flight `mark_terminal` call for
    /// the same entry/target; idempotency makes both orders safe.
    /// Calling it for an already-fully-evicted `entry_id` is also a no-op.
    pub(crate) fn mark_terminal(&self, entry_id: EntryId, target_index: usize) {
        let mut guard = self.state.lock().unwrap();
        if mark_terminal_locked(&mut guard, entry_id.0, target_index) {
            self.cond.notify_all();
        }
    }

    /// Bulk panic-safety hook: marks every currently-live entry terminal
    /// for `target_index`, as if that target's lane called
    /// [`mark_terminal`](Self::mark_terminal) on each of its still-pending
    /// entries. Intended to be called from a panic/drop-guard in a target
    /// lane worker so a mid-drain panic still releases spool capacity for
    /// every entry that lane was holding, instead of leaking it.
    ///
    /// Safe to call even for entries `target_index` was never pending on
    /// (a no-op for those, via the same idempotency as `mark_terminal`) and
    /// safe to call more than once.
    pub(crate) fn mark_all_remaining_terminal_for_target(&self, target_index: usize) {
        let mut guard = self.state.lock().unwrap();
        let ids: Vec<u64> = guard.entries.keys().copied().collect();
        let mut evicted_any = false;
        for id in ids {
            if mark_terminal_locked(&mut guard, id, target_index) {
                evicted_any = true;
            }
        }
        if evicted_any {
            self.cond.notify_all();
        }
    }

    /// Explicitly closes the store, removing the run-scoped directory
    /// (spool files and lockfile). Safe to call more than once, and safe to
    /// skip: [`Drop`] calls it automatically at run end.
    pub(crate) fn close(&self) -> std::io::Result<()> {
        if self.closed.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        // Best-effort: release the job-scoped lock claimed by
        // `acquire_job_lock` so a subsequent `open()` for the same job
        // isn't blocked by this run's own (now-finished) claim.
        let _ = fs::remove_file(&self.job_lock_path);
        fs::remove_dir_all(&self.run_dir)
    }
}

impl Drop for SpoolStore {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

/// Removes `id` from `guard.entries`, deletes its spool file, releases its
/// bytes, and decrements the live-entry count. No-op if `id` is unknown
/// (already evicted).
fn evict_locked(guard: &mut Inner, id: u64) {
    if let Some(entry) = guard.entries.remove(&id) {
        let _ = fs::remove_file(&entry.path);
        guard.used = guard.used.saturating_sub(entry.size);
        guard.live_entry_count = guard.live_entry_count.saturating_sub(1);
    }
}

/// Removes `target_index` from entry `id`'s remaining-targets set (a no-op
/// if already removed or the entry is already evicted) and evicts the entry
/// if that was the last pending target. Returns whether an eviction
/// happened (so the caller knows whether to wake waiters).
fn mark_terminal_locked(guard: &mut Inner, id: u64, target_index: usize) -> bool {
    let should_evict = match guard.entries.get_mut(&id) {
        Some(entry) => {
            entry.remaining_targets.remove(&target_index) && entry.remaining_targets.is_empty()
        }
        None => false,
    };
    if should_evict {
        evict_locked(guard, id);
    }
    should_evict
}

#[cfg(unix)]
fn is_pid_alive(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }
    // signal 0: no signal sent, just an existence/permission check.
    let ret = unsafe { libc::kill(pid, 0) };
    if ret == 0 {
        return true;
    }
    match std::io::Error::last_os_error().raw_os_error() {
        Some(errno) if errno == libc::ESRCH => false,
        // EPERM means the process exists but we can't signal it: alive.
        // Any other ambiguous error: assume alive to avoid destructively
        // deleting a directory we're not sure is orphaned.
        _ => true,
    }
}

#[cfg(not(unix))]
fn is_pid_alive(_pid: i32) -> bool {
    // Conservative: without a portable liveness check, assume alive so we
    // never delete a directory that might still be in use.
    true
}

fn generate_run_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{nanos:x}-{}", std::process::id())
}

/// Parses the pid from a sibling's lockfile content (its first line).
fn sibling_pid(lock_contents: &str) -> Option<i32> {
    lock_contents.lines().next()?.trim().parse::<i32>().ok()
}

/// Decides whether a sibling directory named `dir_name` belongs to
/// `job_name`, by EXACT identity match rather than a directory-name prefix
/// match (which would let a shorter job name swallow a longer sibling job's
/// directories, e.g. `backup` matching `backup-nightly-<run_id>`).
///
/// Primary check: if `lock_contents` is available and its second line (the
/// job name, written by [`SpoolStore::open_after_job_lock`]) is present,
/// that is authoritative -- exact string equality against `job_name`.
///
/// Fallback (used when the lockfile is missing/unreadable -- the orphan
/// case -- or is an old-format lockfile with no embedded job name):
/// recover the job name from `dir_name` itself. `generate_run_id` produces
/// run ids of the form `<nanos_hex>-<pid>` (two trailing hyphen-delimited
/// components), so this tries stripping both one and two trailing
/// components and accepts an exact match against either -- covering both
/// real run ids and simpler single-component run ids (as used by this
/// module's own tests) -- while still requiring exact equality, never a
/// prefix match, against `job_name`.
fn sibling_belongs_to_job(dir_name: &str, lock_contents: Option<&str>, job_name: &str) -> bool {
    if let Some(embedded) = lock_contents.and_then(|c| c.lines().nth(1)) {
        return embedded == job_name;
    }

    [1usize, 2usize]
        .into_iter()
        .filter_map(|trailing_segments| strip_trailing_hyphen_segments(dir_name, trailing_segments))
        .any(|candidate| candidate == job_name)
}

/// Strips `count` trailing `-`-delimited segments from `name`, returning
/// the remainder, or `None` if `name` doesn't have that many segments.
fn strip_trailing_hyphen_segments(name: &str, count: usize) -> Option<&str> {
    let mut end = name.len();
    for _ in 0..count {
        end = name[..end].rfind('-')?;
    }
    Some(&name[..end])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::Barrier;
    use std::sync::atomic::AtomicU32;
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration as StdDuration;

    static TEMP_COUNTER: AtomicU32 = AtomicU32::new(0);

    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new(prefix: &str) -> Self {
            let unique = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system time before unix epoch")
                .as_nanos();
            let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "pathsync-spool-{prefix}-{unique}-{}-{counter}",
                std::process::id()
            ));
            fs::create_dir_all(&path).expect("failed to create temp dir");
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn write_spool_file(store: &SpoolStore, name: &str, contents: &[u8]) -> PathBuf {
        let path = store.run_dir().join(name);
        fs::write(&path, contents).expect("failed to write spool file");
        path
    }

    #[test]
    fn happy_path_two_targets_release_capacity_and_delete_file() {
        let root = TempDir::new("happy");
        let store = SpoolStore::open(root.path(), "job", Some(1024), 0).unwrap();

        store.reserve(10).unwrap();
        let path = write_spool_file(&store, "a.bin", &[0u8; 10]);
        let id = store.register(path.clone(), 10, [0usize, 1usize]);

        assert!(path.exists());

        store.mark_terminal(id, 0);
        assert!(
            path.exists(),
            "file must survive until all targets terminal"
        );

        store.mark_terminal(id, 1);
        assert!(
            !path.exists(),
            "file must be deleted once all targets terminal"
        );

        // Capacity released: a fresh reservation for the full cap succeeds
        // immediately without blocking.
        store.reserve(1024).unwrap();
    }

    #[test]
    fn reservation_larger_than_cap_rejected_immediately() {
        let root = TempDir::new("toobig");
        let store = SpoolStore::open(root.path(), "job", Some(100), 0).unwrap();

        let err = store.reserve(101).unwrap_err();
        assert!(matches!(
            err,
            SpoolError::ExceedsCapacity {
                requested: 101,
                cap: 100
            }
        ));
    }

    #[test]
    fn mark_terminal_on_failure_outcome_also_evicts() {
        let root = TempDir::new("fail");
        let store = SpoolStore::open(root.path(), "job", Some(1024), 0).unwrap();

        store.reserve(5).unwrap();
        let path = write_spool_file(&store, "f.bin", &[1u8; 5]);
        let id = store.register(path.clone(), 5, [0usize]);

        // A "failure" terminal outcome is just another mark_terminal call:
        // the store doesn't distinguish success/failure, only pending vs
        // terminal.
        store.mark_terminal(id, 0);
        assert!(!path.exists());
        store.reserve(1024).unwrap();
    }

    #[test]
    fn double_mark_terminal_same_entry_and_target_is_idempotent() {
        let root = TempDir::new("idempotent");
        let store = SpoolStore::open(root.path(), "job", Some(1024), 0).unwrap();

        store.reserve(5).unwrap();
        let path = write_spool_file(&store, "g.bin", &[2u8; 5]);
        let id = store.register(path.clone(), 5, [0usize, 1usize]);

        store.mark_terminal(id, 0);
        // Second call for the same (entry, target) pair must not panic and
        // must not double-decrement (which would otherwise cause the entry
        // to evict after only one of its two targets reported).
        store.mark_terminal(id, 0);
        assert!(
            path.exists(),
            "still one pending target after idempotent repeat"
        );

        store.mark_terminal(id, 1);
        assert!(!path.exists());

        // Calling again after full eviction is also a safe no-op.
        store.mark_terminal(id, 0);
        store.mark_terminal(id, 1);
    }

    #[test]
    fn zero_live_entries_and_zero_outstanding_claims_fails_fast_instead_of_hanging() {
        let root = TempDir::new("deadlock");
        // No cap, so capacity itself is never the constraint (with a
        // correctly-resolved reserve()/register() pairing, capacity alone
        // can never be the bottleneck when both live_entry_count and
        // outstanding_claims are zero -- see `reserve`'s doc comment). Set
        // min_free_bytes higher than the volume's real available space so
        // it can genuinely never be satisfied: with nothing registered and
        // nothing outstanding, nothing could ever free real disk space, so
        // this must fail fast rather than block.
        let available = fs4::available_space(root.path()).expect("query available space");
        let store =
            SpoolStore::open(root.path(), "job", None, available + 1024 * 1024 * 1024).unwrap();

        let (tx, rx) = mpsc::channel();
        let store = Arc::new(store);
        let store_clone = Arc::clone(&store);
        thread::spawn(move || {
            let result = store_clone.reserve(1);
            let _ = tx.send(result);
        });

        let result = rx
            .recv_timeout(StdDuration::from_secs(5))
            .expect("reserve() must return promptly instead of hanging");
        assert!(matches!(
            result,
            Err(SpoolError::WouldDeadlock { requested: 1 })
        ));
    }

    #[test]
    fn reserve_blocks_instead_of_spuriously_failing_while_a_claim_is_outstanding_but_unregistered()
    {
        // Regresses the P0 race: reserve() used to fast-fail with
        // WouldDeadlock whenever live_entry_count was zero, even though a
        // just-granted reservation that hasn't been register()'d yet (the
        // real, non-trivial window while stage.rs copies a whole file
        // before registering) could still resolve into an evictable entry.
        let root = TempDir::new("outstanding-claim");
        let store = Arc::new(SpoolStore::open(root.path(), "job", Some(10), 0).unwrap());

        // First reservation claims the entire cap but is deliberately NOT
        // yet registered, simulating the in-flight window.
        store.reserve(10).unwrap();

        let (tx, rx) = mpsc::channel();
        let store_clone = Arc::clone(&store);
        thread::spawn(move || {
            // Must BLOCK, not spuriously fail: a claim is still outstanding
            // and could resolve into an evictable entry.
            let result = store_clone.reserve(5);
            let _ = tx.send(result);
        });

        // Prove it is genuinely still blocked (not merely about to answer):
        // under the old buggy check this would already have sent an
        // `Err(WouldDeadlock)` well within this window.
        let early = rx.recv_timeout(StdDuration::from_millis(300));
        assert!(
            early.is_err(),
            "reserve() must block while a claim is outstanding, not fail fast: got {early:?}"
        );

        // Resolve the first claim exactly as stage_file's real success path
        // does: register (converting the claim into a tracked entry), then
        // mark it terminal (evicting it and freeing capacity).
        let path = write_spool_file(&store, "claim.bin", &[7u8; 10]);
        let id = store.register(path, 10, [0usize]);
        store.mark_terminal(id, 0);

        let result = rx
            .recv_timeout(StdDuration::from_secs(5))
            .expect("blocked reserve() must proceed once the outstanding claim resolves, not hang");
        assert!(result.is_ok(), "expected success, got {result:?}");
    }

    #[test]
    fn blocking_reservation_proceeds_after_concurrent_eviction() {
        let root = TempDir::new("blocking");
        let store = Arc::new(SpoolStore::open(root.path(), "job", Some(10), 0).unwrap());

        store.reserve(10).unwrap();
        let path = write_spool_file(&store, "block.bin", &[3u8; 10]);
        let id = store.register(path, 10, [0usize]);

        let (tx, rx) = mpsc::channel();
        let store_clone = Arc::clone(&store);
        thread::spawn(move || {
            // Must block: cap is full and nothing is free yet.
            let result = store_clone.reserve(10);
            let _ = tx.send(result);
        });

        // Give the waiter a moment to actually start blocking, then evict
        // to free capacity.
        thread::sleep(StdDuration::from_millis(200));
        store.mark_terminal(id, 0);

        let result = rx
            .recv_timeout(StdDuration::from_secs(5))
            .expect("blocked reserve() must unblock after the eviction, not hang");
        assert!(result.is_ok());
    }

    #[test]
    fn panic_safety_hook_releases_all_pending_entries_for_a_target() {
        let root = TempDir::new("panic-hook");
        let store = SpoolStore::open(root.path(), "job", Some(1024), 0).unwrap();

        store.reserve(30).unwrap();
        let p1 = write_spool_file(&store, "p1.bin", &[4u8; 10]);
        let p2 = write_spool_file(&store, "p2.bin", &[5u8; 10]);
        let p3 = write_spool_file(&store, "p3.bin", &[6u8; 10]);
        // Target 0 is pending on all three entries; target 1 only on p2.
        let id1 = store.register(p1.clone(), 10, [0usize]);
        let id2 = store.register(p2.clone(), 10, [0usize, 1usize]);
        let id3 = store.register(p3.clone(), 10, [0usize]);

        // Simulate "target 0's worker died with N entries still pending
        // for it."
        store.mark_all_remaining_terminal_for_target(0);

        assert!(!p1.exists(), "entry only pending on target 0 must evict");
        assert!(p2.exists(), "entry still pending on target 1 must survive");
        assert!(!p3.exists(), "entry only pending on target 0 must evict");

        // The still-alive target can still terminate its own entry normally.
        store.mark_terminal(id2, 1);
        assert!(!p2.exists());

        // Calling the hook again is safe (already-released ids are no-ops).
        store.mark_all_remaining_terminal_for_target(0);
        let _ = (id1, id3);

        // Capacity fully released.
        store.reserve(1024).unwrap();
    }

    #[test]
    fn orphan_dead_pid_sibling_is_removed_on_construction() {
        let root = TempDir::new("orphan-dead");
        let sibling = root.path().join("job-oldrun");
        fs::create_dir_all(&sibling).unwrap();

        // Reap a short-lived child so its pid is guaranteed not to be
        // running anymore, without relying on an arbitrary "unlikely" pid.
        let mut child = std::process::Command::new("true")
            .spawn()
            .expect("failed to spawn helper process");
        let dead_pid = child.id();
        child.wait().expect("failed to reap helper process");

        fs::write(sibling.join(LOCK_FILE_NAME), dead_pid.to_string()).unwrap();

        let _store = SpoolStore::open(root.path(), "job", None, 0).unwrap();
        assert!(!sibling.exists(), "dead-pid sibling must be cleaned up");
    }

    #[test]
    fn orphan_missing_lockfile_sibling_is_removed_on_construction() {
        let root = TempDir::new("orphan-missing-lock");
        let sibling = root.path().join("job-oldrun");
        fs::create_dir_all(&sibling).unwrap();
        // No lockfile at all.

        let _store = SpoolStore::open(root.path(), "job", None, 0).unwrap();
        assert!(
            !sibling.exists(),
            "sibling with no lockfile must be cleaned up"
        );
    }

    #[test]
    fn live_lock_sibling_fails_construction_and_is_left_untouched() {
        let root = TempDir::new("live-lock");
        let sibling = root.path().join("job-liverun");
        fs::create_dir_all(&sibling).unwrap();
        fs::write(sibling.join(LOCK_FILE_NAME), std::process::id().to_string()).unwrap();

        let err = SpoolStore::open(root.path(), "job", None, 0).err().unwrap();
        assert!(matches!(err, SpoolError::ConcurrentRunDetected { .. }));
        assert!(
            sibling.exists(),
            "live-locked sibling must be left untouched"
        );
        assert!(
            sibling.join(LOCK_FILE_NAME).exists(),
            "live-locked sibling's lockfile must be left untouched"
        );
    }

    #[test]
    fn concurrent_open_calls_for_same_job_never_both_succeed() {
        // Regresses the P1 TOCTOU race: two `open()` calls for the same job
        // started close together used to both pass the (pure filesystem
        // read) live-lock check before either had written its own
        // lockfile, so both could proceed concurrently against the same
        // job. Run several iterations with a `Barrier` to make the race
        // window as tight as possible each time, so a regression is caught
        // reliably rather than passing by accident of OS scheduling.
        for iteration in 0..30 {
            let root = TempDir::new(&format!("concurrent-open-{iteration}"));
            let root_path = root.path().to_path_buf();
            let barrier = Arc::new(Barrier::new(2));

            let barrier1 = Arc::clone(&barrier);
            let root1 = root_path.clone();
            let h1 = thread::spawn(move || {
                barrier1.wait();
                SpoolStore::open(&root1, "job", None, 0)
            });

            let barrier2 = Arc::clone(&barrier);
            let root2 = root_path.clone();
            let h2 = thread::spawn(move || {
                barrier2.wait();
                SpoolStore::open(&root2, "job", None, 0)
            });

            let r1 = h1.join().expect("open() thread 1 must not panic");
            let r2 = h2.join().expect("open() thread 2 must not panic");

            let is_concurrent_err = |r: &Result<SpoolStore, SpoolError>| {
                matches!(r, Err(SpoolError::ConcurrentRunDetected { .. }))
            };

            assert!(
                r1.is_ok() ^ r2.is_ok(),
                "iteration {iteration}: exactly one of the two concurrent open() calls must succeed"
            );
            assert!(
                is_concurrent_err(&r1) || is_concurrent_err(&r2),
                "iteration {iteration}: the losing open() call must fail with ConcurrentRunDetected"
            );
        }
    }

    #[test]
    fn hyphenated_job_name_prefix_collision_does_not_cross_match() {
        // Regresses P1 #5: job "backup"'s prefix `backup-` used to
        // raw-prefix-match sibling directories belonging to a DIFFERENT job
        // "backup-nightly" (e.g. `backup-nightly-<run_id>`), one
        // -directionally, causing false live-lock conflicts and wrongful
        // orphan deletion of backup-nightly's spool directories.
        let root = TempDir::new("prefix-collision");

        // A live "backup-nightly" run (current two-line lockfile format:
        // pid, then job name).
        let nightly_live = root.path().join("backup-nightly-liverun");
        fs::create_dir_all(&nightly_live).unwrap();
        fs::write(
            nightly_live.join(LOCK_FILE_NAME),
            format!("{}\nbackup-nightly\n", std::process::id()),
        )
        .unwrap();

        // A crashed "backup-nightly" run (dead pid): would be wrongly
        // deleted as an "orphan of `backup`" under a raw prefix match.
        let mut child = std::process::Command::new("true")
            .spawn()
            .expect("failed to spawn helper process");
        let dead_pid = child.id();
        child.wait().expect("failed to reap helper process");
        let nightly_dead = root.path().join("backup-nightly-deadrun");
        fs::create_dir_all(&nightly_dead).unwrap();
        fs::write(
            nightly_dead.join(LOCK_FILE_NAME),
            format!("{dead_pid}\nbackup-nightly\n"),
        )
        .unwrap();

        // Job "backup" (the shorter, prefix-colliding name) must not treat
        // either sibling as its own: no false live-lock conflict from the
        // live one, and no wrongful cleanup of the dead one.
        let store = SpoolStore::open(root.path(), "backup", None, 0)
            .expect("job `backup` must not see `backup-nightly`'s live run as a conflict");

        assert!(
            nightly_live.exists(),
            "backup-nightly's live run must be left untouched by job `backup`"
        );
        assert!(
            nightly_dead.exists(),
            "backup-nightly's crashed run must not be wrongly deleted by job `backup`'s orphan cleanup"
        );

        drop(store);
    }

    #[test]
    fn unrelated_job_and_foreign_files_are_left_alone() {
        let root = TempDir::new("unrelated");

        let other_job_dead = root.path().join("otherjob-run1");
        fs::create_dir_all(&other_job_dead).unwrap();
        // No lockfile: would be an orphan if it belonged to `job`, but it
        // doesn't share the prefix so it must be left alone.

        let other_job_live = root.path().join("otherjob-run2");
        fs::create_dir_all(&other_job_live).unwrap();
        fs::write(
            other_job_live.join(LOCK_FILE_NAME),
            std::process::id().to_string(),
        )
        .unwrap();

        let foreign_file = root.path().join("readme.txt");
        fs::write(&foreign_file, b"not a spool dir").unwrap();

        let _store = SpoolStore::open(root.path(), "job", None, 0).unwrap();

        assert!(other_job_dead.exists(), "different job's dir left alone");
        assert!(
            other_job_live.exists(),
            "different job's live dir left alone"
        );
        assert!(foreign_file.exists(), "unrelated file left alone");
    }

    #[test]
    fn run_dir_removed_on_close_and_drop() {
        let root = TempDir::new("close");
        let store = SpoolStore::open(root.path(), "job", None, 0).unwrap();
        let run_dir = store.run_dir().to_path_buf();
        assert!(run_dir.exists());

        store.close().unwrap();
        assert!(!run_dir.exists());

        // Safe to call again (Drop will also call it).
        store.close().unwrap();
    }
}
