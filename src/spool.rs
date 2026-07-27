//! Spool store: run-scoped spool directory, capacity accounting with
//! blocking reservation, refcounted entries, eviction, and orphan cleanup.
//!
//! This module is intentionally UI-free: it knows nothing about progress
//! events or rendering and is fully unit-testable with temp directories.

use std::collections::{HashMap, HashSet};
use std::fs;
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
pub struct EntryId(u64);

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
}

/// Owns a run-scoped spool directory on internal storage: capacity
/// accounting with blocking reservation, refcounted entries, eviction, and
/// orphan cleanup of stale sibling run directories from crashed runs.
pub struct SpoolStore {
    run_dir: PathBuf,
    spool_root: PathBuf,
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
    pub fn open(
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

        Self::clean_orphans_and_check_live_lock(spool_root, job_name)?;

        let run_id = generate_run_id();
        let run_dir = spool_root.join(format!("{job_name}-{run_id}"));
        fs::create_dir_all(&run_dir).map_err(|e| SpoolError::Io {
            op: "create run directory",
            path: run_dir.clone(),
            source: e,
        })?;
        let lock_path = run_dir.join(LOCK_FILE_NAME);
        fs::write(&lock_path, std::process::id().to_string()).map_err(|e| SpoolError::Io {
            op: "write lockfile",
            path: lock_path,
            source: e,
        })?;

        Ok(Self {
            run_dir,
            spool_root: spool_root.to_path_buf(),
            cap: max_bytes,
            min_free_bytes,
            state: Mutex::new(Inner {
                used: 0,
                entries: HashMap::new(),
                next_entry_id: 0,
                live_entry_count: 0,
            }),
            cond: Condvar::new(),
            closed: AtomicBool::new(false),
        })
    }

    /// Two-pass sibling scan: first detect whether any same-job sibling
    /// holds a live lock (failing fast, touching nothing, if so), then
    /// remove orphaned siblings (absent or dead-pid lockfile).
    fn clean_orphans_and_check_live_lock(
        spool_root: &Path,
        job_name: &str,
    ) -> Result<(), SpoolError> {
        let prefix = format!("{job_name}-");
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
            if !name.starts_with(&prefix) {
                continue;
            }

            let lock_path = path.join(LOCK_FILE_NAME);
            match fs::read_to_string(&lock_path) {
                Ok(contents) => match contents.trim().parse::<i32>() {
                    Ok(pid) if is_pid_alive(pid) => {
                        return Err(SpoolError::ConcurrentRunDetected {
                            job_name: job_name.to_string(),
                            pid,
                            path,
                        });
                    }
                    _ => orphans.push(path),
                },
                Err(_) => orphans.push(path),
            }
        }

        for path in orphans {
            let _ = fs::remove_dir_all(&path);
        }

        Ok(())
    }

    /// The run-scoped spool directory for this store.
    pub fn run_dir(&self) -> &Path {
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
    /// - the reservation cannot currently be satisfied and there are zero
    ///   live (unevicted) entries in the store, meaning nothing could
    ///   possibly be evicted to free space
    ///   ([`SpoolError::WouldDeadlock`]).
    pub fn reserve(&self, bytes: u64) -> Result<(), SpoolError> {
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
                return Ok(());
            }

            if guard.live_entry_count == 0 {
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
    pub fn register(
        &self,
        path: PathBuf,
        size: u64,
        pending_targets: impl IntoIterator<Item = usize>,
    ) -> EntryId {
        let mut guard = self.state.lock().unwrap();
        let id = guard.next_entry_id;
        guard.next_entry_id += 1;

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
    pub fn mark_terminal(&self, entry_id: EntryId, target_index: usize) {
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
    pub fn mark_all_remaining_terminal_for_target(&self, target_index: usize) {
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
    pub fn close(&self) -> std::io::Result<()> {
        if self.closed.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
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
    fn zero_live_entries_fails_fast_instead_of_hanging() {
        let root = TempDir::new("deadlock");
        // Cap is 0, min-free is 0: any positive reservation can never be
        // satisfied by capacity, and nothing is registered to evict.
        let store = SpoolStore::open(root.path(), "job", Some(10), 0).unwrap();

        // Fill capacity with a bare reservation (no registered entry, so
        // live_entry_count stays 0): simulates "capacity exhausted with
        // nothing outstanding to evict."
        store.reserve(10).unwrap();

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
