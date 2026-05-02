# Streaming Verification Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add trustworthy per-target copy verification without a pre-copy freeze by hashing sources inside copy workers, streaming successful copies to bounded verifier workers, and reporting target-level copied/verified results.

**Architecture:** Planning stays fast and produces copy intent only. Runtime copy workers own source signature capture, source stability checks, temp copy, rename, and enqueue successful transfers into a bounded verification queue. Verification workers run concurrently, read destinations only, update the same transfer lifecycle rows, and final completion is based on verified transfers.

**Tech Stack:** Rust, crossbeam-channel, xxhash-rust `xxh3_128`, existing `copy.rs` scheduler/UI, existing progress model/format tests, cargo test/fmt/clippy.

---

## Decisions Locked In

- Verification is always on.
- Source signature is computed inside the copy worker just before copying.
- Signature is `xxh3_128` plus size, not cryptographic.
- Same-source multi-target transfers share a source-signature cache.
- Same-source target copies wait for the shared source hash, then copy in parallel.
- Source metadata is checked before and after each target copy.
- If source changes, fail affected non-verified transfers and remove any destination written by this run.
- Destination verification reads only destinations.
- Verification workers are separate internal executors and run as soon as copy successes are enqueued.
- Copy workers do not wait for all verification, but they can block on a bounded verification queue.
- Live rows represent transfer lifecycle, not literal workers: `hashing -> copying -> verifying -> done`.
- Visible row prefixes become `T01`, `T02`, etc.
- Main progress and ETA are based on verified bytes/transfers.
- Small files suppress noisy hash/verify live states unless slow or failed.
- Final summary uses `Target Results`, not `Verification`.
- Skipped-existing files are not verified in this implementation.

## File Map

- `Cargo.toml`, `Cargo.lock`: switch dependency use to `xxh3_128` under existing `xxhash-rust`.
- `src/plan.rs`: remove mandatory planning-time signatures and full-file collision comparison; keep `TransferPlan` as copy intent only.
- `src/copy.rs`: add runtime signature cache, completed-transfer queue, verifier workers, source stability checks, transfer lifecycle events, target result aggregation.
- `src/error.rs`: add operation variants for hash/source-change/verify.
- `src/progress_model.rs`: replace worker-row-only model with transfer row state fields, add copied/verified metrics and target result rows.
- `src/progress_format.rs`: render `Txx` transfer lifecycle rows and `Target Results`.
- `src/lib.rs`: update preview UI model to show lifecycle rows and target results.
- `tests/plan_layout.rs`: assert planning does not hash sources and skipped files do not require source reads.
- `tests/copy_integration.rs`: cover successful copy+verify, verify failure, source-changed handling, and multi-target target results.
- `tests/progress_model.rs`, `tests/progress_format.rs`: cover lifecycle states, `Txx` prefixes, verified-based progress, target results.
- `README.md`: document verification semantics and trust boundaries.

## Task 0: Stabilize Current Workspace

**Files:**
- Modify: `src/plan.rs`
- Modify: `src/copy.rs`
- Modify: `tests/plan_layout.rs`
- Inspect: `git diff`

- [ ] **Step 1: Inspect partial edits**

Run:

```bash
git diff -- src/plan.rs src/copy.rs tests/plan_layout.rs
```

Expected: identify the interrupted async-prep changes, especially `Option<FileSignature>`, `PhaseKind::Preparing`, and any tests still expecting planning-time signatures.

- [ ] **Step 2: Normalize plan state**

Edit `src/plan.rs` so `TransferPlan` has no runtime signature field:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferPlan {
    pub source: PathBuf,
    pub dest: PathBuf,
    pub size: u64,
    pub display_name: String,
}
```

Keep `FileSignature` only if it is used by runtime code, or move it to `copy.rs` in Task 2.

- [ ] **Step 3: Remove preparation-phase events**

Edit `src/copy.rs` to remove these partial concepts:

```rust
PhaseKind::Preparing
WorkerEvent::PreparationFileStarted
WorkerEvent::PreparationProgress
WorkerEvent::PreparationFileFinished
prepare_plan_signatures(...)
```

Expected: the code returns to copy-worker-driven execution before adding the new runtime lifecycle model.

- [ ] **Step 4: Verify the workspace compiles before feature work**

Run:

```bash
cargo test --no-run
```

Expected: compilation succeeds, or failures are limited to tests that must be updated because `TransferPlan` no longer has `signature`.

## Task 1: Planning Must Not Hash Full Files

**Files:**
- Modify: `src/plan.rs`
- Test: `tests/plan_layout.rs`

- [ ] **Step 1: Write/adjust failing planning tests**

In `tests/plan_layout.rs`, keep these behaviors:

```rust
#[test]
fn build_plan_defers_source_signature_until_runtime() {
    let temp = TempDir::new();
    let source = temp.path().join("source");
    let target = temp.path().join("target");
    fs::create_dir_all(&source).unwrap();
    fs::create_dir_all(&target).unwrap();
    write_file(&source.join("photo.jpg"), b"source bytes");

    let job = PlanJob {
        source: source.clone(),
        targets: vec![target],
        extensions: vec!["jpg".to_string()],
        compare_policy: ComparePolicy::PathSize,
        template: "{filename}".to_string(),
    };

    let build = build_plan(&job, false, |path, _metadata| {
        Ok(sample_context(path.file_name().and_then(|name| name.to_str()).unwrap()))
    }).unwrap();

    assert_eq!(build.plans.len(), 1);
    assert_eq!(build.plans[0].size, 12);
}
```

For Unix, keep the skipped-destination test:

```rust
#[cfg(unix)]
#[test]
fn build_plan_does_not_hash_skipped_source_contents() {
    // Create same-size destination so PathSize skips.
    // chmod source to 000.
    // build_plan must succeed with no plans and one skipped file when using a
    // test context callback that does not read EXIF.
}
```

- [ ] **Step 2: Run tests and verify failure if planning still hashes**

Run:

```bash
cargo test --test plan_layout build_plan_does_not_read_skipped_source_contents
```

Expected before fix: FAIL if planning still hashes or performs full-file content comparison before skip checks.

- [ ] **Step 3: Implement fast planning**

Ensure `build_plan` only stats sources and destinations and uses the provided context callback. It must not call a signature helper, hash helper, or full-file content comparison. Existing EXIF metadata reads in the higher-level context callback remain allowed because destination layout rendering depends on them.

- [ ] **Step 4: Verify planning tests**

Run:

```bash
cargo test --test plan_layout
```

Expected: all `plan_layout` tests pass.

## Task 2: Runtime Signature Types and Source Stability

**Files:**
- Modify: `src/copy.rs`
- Modify: `src/error.rs`
- Test: unit tests inside `src/copy.rs`

- [ ] **Step 1: Add runtime types**

In `src/copy.rs`, define:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileSignature {
    size: u64,
    xxh3_128: u128,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct SourceSignatureKey {
    path: PathBuf,
    size: u64,
    mtime: Option<std::time::SystemTime>,
    #[cfg(unix)]
    dev: u64,
    #[cfg(unix)]
    ino: u64,
}

#[derive(Debug, Clone)]
struct SignedSource {
    key: SourceSignatureKey,
    signature: FileSignature,
}

#[derive(Debug, Clone)]
struct CompletedTransfer {
    source: PathBuf,
    dest: PathBuf,
    target: PathBuf,
    display_name: String,
    size: u64,
    expected: FileSignature,
}
```

- [ ] **Step 2: Add copy operations**

In `src/error.rs`, add:

```rust
#[error("hash_source")]
HashSource,
#[error("source_changed")]
SourceChanged,
#[error("verify")]
Verify,
```

- [ ] **Step 3: Implement source metadata key helper**

In `src/copy.rs`, add:

```rust
fn source_signature_key(path: &Path) -> io::Result<SourceSignatureKey> {
    let metadata = fs::metadata(path)?;
    Ok(SourceSignatureKey {
        path: path.to_path_buf(),
        size: metadata.len(),
        mtime: metadata.modified().ok(),
        #[cfg(unix)]
        dev: {
            use std::os::unix::fs::MetadataExt;
            metadata.dev()
        },
        #[cfg(unix)]
        ino: {
            use std::os::unix::fs::MetadataExt;
            metadata.ino()
        },
    })
}

fn ensure_source_unchanged(path: &Path, expected: &SourceSignatureKey) -> io::Result<()> {
    let actual = source_signature_key(path)?;
    if &actual == expected {
        Ok(())
    } else {
        Err(io::Error::new(ErrorKind::InvalidData, "source changed during run"))
    }
}
```

- [ ] **Step 4: Add xxh3_128 helper**

Use `xxhash_rust::xxh3::Xxh3` and return `hasher.digest128()`.

```rust
fn hash_file_xxh3_128<F>(path: &Path, size: u64, mut progress: F) -> io::Result<FileSignature>
where
    F: FnMut(u64),
{
    const BUFFER_SIZE: usize = 1024 * 1024;
    let file = fs::File::open(path)?;
    let mut reader = std::io::BufReader::new(file);
    let mut hasher = xxhash_rust::xxh3::Xxh3::new();
    let mut buffer = [0_u8; BUFFER_SIZE];
    let mut done = 0_u64;
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        done += read as u64;
        progress(done);
    }
    Ok(FileSignature { size, xxh3_128: hasher.digest128() })
}
```

- [ ] **Step 5: Unit test source-change detection**

Add a unit test that writes a temp source, captures key, modifies contents or mtime, and expects `ensure_source_unchanged` to return `InvalidData`.

Run:

```bash
cargo test source_changed
```

Expected: PASS after helper implementation.

## Task 3: Shared Source Signature Cache

**Files:**
- Modify: `src/copy.rs`
- Test: unit tests inside `src/copy.rs`

- [ ] **Step 1: Add cache state**

In `src/copy.rs`, add:

```rust
type SourceSignatureCache = std::sync::Arc<std::sync::Mutex<std::collections::HashMap<SourceSignatureKey, SourceSignatureEntry>>>;

enum SourceSignatureEntry {
    Ready(SignedSource),
    Failed(String),
}
```

For the first implementation, use a `Mutex<HashMap<...>>` and compute outside the lock:

```rust
fn get_or_compute_source_signature(
    cache: &SourceSignatureCache,
    plan: &TransferPlan,
    progress: impl FnMut(u64),
) -> Result<SignedSource, CopyFailure>
```

Implementation rule:
- Compute key before hash.
- Check cache.
- If absent, hash file.
- Check source unchanged after hash.
- Insert `Ready`.
- If another worker already inserted while this worker hashed, use the cache entry and discard duplicate result.

Note: this simple first version may duplicate hash work during races. If that happens in tests or real runs, upgrade to an in-progress condvar entry. Do not block this task on perfect cache internals unless duplicate hashing is observed.

- [ ] **Step 2: Add cache test for reuse**

Write a unit test with two plans pointing at the same temp source and different destinations. Call `get_or_compute_source_signature` twice and assert returned signatures are equal.

Run:

```bash
cargo test source_signature_cache
```

Expected: PASS.

- [ ] **Step 3: Decide if condvar is required**

If duplicate-hash avoidance must be strict, replace entry with:

```rust
enum SourceSignatureEntry {
    InProgress(std::sync::Arc<(std::sync::Mutex<Option<Result<SignedSource, String>>>, std::sync::Condvar)>),
    Ready(SignedSource),
    Failed(String),
}
```

Expected behavior:
- First worker owns hash.
- Same-source waiters block and show `hashing` with still bar.
- All waiters proceed after `Ready`.

## Task 4: Copy Worker Lifecycle

**Files:**
- Modify: `src/copy.rs`
- Test: unit tests and `tests/copy_integration.rs`

- [ ] **Step 1: Extend worker events**

Replace copy-only events with transfer lifecycle events:

```rust
enum TransferPhase {
    Hashing,
    Copying,
    Verifying,
}

enum WorkerEvent {
    TransferStarted { transfer_id: usize, phase: TransferPhase, name: String, source: PathBuf, dest: PathBuf, total: u64 },
    TransferProgress { transfer_id: usize, phase: TransferPhase, done: u64 },
    TransferPhaseChanged { transfer_id: usize, phase: TransferPhase, total: u64 },
    TransferCopied { transfer_id: usize, bytes: u64 },
    TransferVerified { transfer_id: usize, bytes: u64 },
    TransferFailed { transfer_id: usize, failure: CopyFailure },
    PhaseStarted { phase: PhaseKind, worker_count: usize },
}
```

- [ ] **Step 2: Assign transfer IDs**

Add an atomic transfer ID in `run_copy`:

```rust
let next_transfer_id = Arc::new(AtomicUsize::new(1));
```

Each scheduled `TransferPlan` gets a stable `transfer_id` before entering `run_plan`.

- [ ] **Step 3: Hash inside `run_plan`**

Inside `run_plan`:

```rust
send Hashing state for large files only;
let signed_source = get_or_compute_source_signature(...)?;
ensure_source_unchanged(&plan.source, &signed_source.key)?;
send Copying state;
copy_file(...)?;
ensure_source_unchanged(&plan.source, &signed_source.key)?;
enqueue CompletedTransfer;
send TransferCopied;
```

For small files, still hash before copy but skip live `Hashing` state unless the operation lasts long enough to render. The first version may simply not emit `Hashing` for files below threshold.

- [ ] **Step 4: Remove bad destination on source-changed-after-copy**

If post-copy metadata check fails after rename:

```rust
let _ = fs::remove_file(&plan.dest);
return Err(copy_failure(plan, CopyOperation::SourceChanged, err, "source changed during run".to_string()));
```

- [ ] **Step 5: Integration test source change**

Add a focused test if practical by exposing helper-level behavior. If integration-level race is hard, unit-test the helper and rely on copy path tests for enqueue behavior.

## Task 5: Streaming Verification Workers

**Files:**
- Modify: `src/copy.rs`
- Test: `tests/copy_integration.rs`

- [ ] **Step 1: Add bounded verification queue**

Use crossbeam bounded channel:

```rust
let queue_capacity = std::cmp::max(16, job.parallel * 4);
let (verify_tx, verify_rx) = crossbeam_channel::bounded::<CompletedTransfer>(queue_capacity);
```

- [ ] **Step 2: Start verifier workers before copy workers**

Default:

```rust
let verify_parallel = std::cmp::min(job.targets.len().max(1), 2);
```

Spawn verifier workers that loop on `verify_rx`.

- [ ] **Step 3: Verify destination only**

Verifier worker:

```rust
fn verify_completed_transfer(transfer: &CompletedTransfer) -> Result<(), CopyFailure> {
    let metadata = fs::metadata(&transfer.dest)?;
    let actual = hash_file_xxh3_128(&transfer.dest, metadata.len(), |done| {
        send TransferProgress { phase: Verifying, done };
    })?;
    if actual == transfer.expected { Ok(()) } else { Err(...) }
}
```

- [ ] **Step 4: Send lifecycle events**

When verifier starts:

```rust
WorkerEvent::TransferPhaseChanged { transfer_id, phase: TransferPhase::Verifying, total: transfer.size }
```

When verifier finishes:

```rust
WorkerEvent::TransferVerified { transfer_id, bytes: transfer.size }
```

- [ ] **Step 5: Ensure run completion waits for verifiers**

After all copy workers finish:

```rust
drop(verify_tx);
join all verifier handles;
drop(event_tx);
```

Expected: run does not report complete until all verification work has drained.

## Task 6: Target Results Aggregation

**Files:**
- Modify: `src/copy.rs`
- Modify: `src/progress_model.rs`
- Test: unit tests inside `src/copy.rs`

- [ ] **Step 1: Add target result state**

In `src/copy.rs`:

```rust
#[derive(Debug, Clone, Default)]
struct TargetResult {
    planned: usize,
    copied: usize,
    verified: usize,
    copy_failed: usize,
    verify_failed: usize,
}
```

- [ ] **Step 2: Initialize planned counts from plans**

Before starting workers:

```rust
fn initial_target_results(plans: &[TransferPlan], targets: &[PathBuf]) -> BTreeMap<String, TargetResult>
```

- [ ] **Step 3: Update counts from events**

Rules:
- `TransferCopied`: `copied += 1`
- copy/hash/source-change failure: `copy_failed += 1`
- `TransferVerified`: `verified += 1`
- verify failure: `verify_failed += 1`

- [ ] **Step 4: Unit test target result accounting**

Create a fake event sequence:

```rust
planned 2
copied 1
verified 1
copy_failed 1
```

Assert target result row matches.

## Task 7: Transfer-Row UI Model

**Files:**
- Modify: `src/progress_model.rs`
- Modify: `src/progress_format.rs`
- Modify: `src/copy.rs`
- Test: `tests/progress_model.rs`, `tests/progress_format.rs`

- [ ] **Step 1: Add lifecycle row model**

In `src/progress_model.rs`:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransferRowPhase {
    Hashing,
    Copying,
    Verifying,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferRowModel {
    pub spinner_frame: Option<char>,
    pub transfer_tag: String,
    pub phase: TransferRowPhase,
    pub percent: usize,
    pub item: String,
    pub size: String,
    pub rate: String,
    pub target: String,
}
```

- [ ] **Step 2: Render `Txx` rows**

Expected output shape:

```text
⠹ T01  hashing    [██████            ]  VID_121.mp4  13.3 GB  980 MB/s (T7)
⠼ T02  copying    [████████          ]  VID_122.mp4  12.1 GB   45 MB/s (Archive)
⠧ T03  verifying  [████████████      ]  VID_119.mp4  13.3 GB  210 MB/s (T7)
```

- [ ] **Step 3: Main progress uses verified bytes**

Model fields:

```rust
copied_transfers
verified_transfers
copied_bytes
verified_bytes
planned_transfers
planned_bytes
```

Main progress text:

```text
Copying  [█████-------------------------]  41.2 GB verified of 110.8 GB   ETA 23m15s
```

- [ ] **Step 4: Stats rail includes copied and verified**

Add metrics:

```text
Copied
Verified
Copy W
Verify W
Targets
```

- [ ] **Step 5: Format tests**

Update `tests/progress_format.rs` to assert:
- rows use `T01`, not `W01`
- row phase label appears
- verifying row is folded into same row format
- main bar text says `verified`

## Task 8: Final Summary

**Files:**
- Modify: `src/progress_model.rs`
- Modify: `src/progress_format.rs`
- Modify: `src/copy.rs`
- Test: `tests/progress_format.rs`, `tests/copy_integration.rs`

- [ ] **Step 1: Add target result model**

In `src/progress_model.rs`:

```rust
pub struct TargetResultModel {
    pub target: String,
    pub planned: usize,
    pub copied: usize,
    pub verified: usize,
    pub copy_failed: usize,
    pub verify_failed: usize,
}
```

- [ ] **Step 2: Render `Target Results`**

Expected:

```text
Target Results
------------------------------------------------------------------------
Target       Planned   Copied   Verified   Copy Fail   Verify Fail
T7               123      123        123           0             0
Archive          123      122        121           1             1
```

- [ ] **Step 3: Failure list includes phase and target**

Expected:

```text
Failures
------------------------------------------------------------------------
File                  Target      Phase        Error
VID_001.mp4           Archive     copy         permission denied
VID_002.mp4           Archive     verify       signature mismatch
```

- [ ] **Step 4: Cap failure preview**

Use an existing preview limit pattern or add:

```rust
const SUMMARY_FAILURE_PREVIEW_LIMIT: usize = 20;
```

If failures exceed limit, render:

```text
Showing 20 of 417 failures.
```

## Task 9: End-to-End Tests

**Files:**
- Modify: `tests/copy_integration.rs`

- [ ] **Step 1: Successful run shows target results**

Add/adjust integration test:

```rust
assert!(output.status.success());
assert!(output.stdout.contains("Target Results"));
assert!(output.stdout.contains("Verified"));
```

- [ ] **Step 2: Multi-target partial failure fails run**

Expected:

```rust
assert!(!output.status.success());
assert!(open.join("photo.jpg").exists());
assert!(!blocked.join("photo.jpg").exists());
assert!(output.stdout.contains("Copy Fail"));
```

- [ ] **Step 3: Verification mismatch fails run**

If direct race injection is hard, unit-test `verify_completed_transfer` with a destination whose contents differ from expected signature.

- [ ] **Step 4: Skipped-existing files are not verified**

Keep rerun behavior:

```rust
assert!(output.stdout.contains("no new files to copy") || output.status.success());
```

Do not require skipped files to be hashed.

## Task 10: Documentation and Final Verification

**Files:**
- Modify: `README.md`

- [ ] **Step 1: Document semantics**

Add bullets:

```markdown
- Source signatures are computed by copy workers immediately before copying.
- Same-source multi-target transfers share one prepared signature.
- Destination verification runs in bounded background workers and reads destinations only.
- Final completion is based on verified planned transfers.
- Skipped-existing files are not verified unless copied in the run.
```

- [ ] **Step 2: Run targeted tests**

Run:

```bash
cargo test --test plan_layout
cargo test --test progress_format
cargo test --test copy_integration
cargo test source_changed
```

Expected: all pass.

- [ ] **Step 3: Run full verification**

Run:

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
just build
```

Expected: all pass.

## Self-Review

- Spec coverage: The plan covers worker-local hashing, shared source cache, source-change detection, streaming verifier workers, folded lifecycle UI, verified-based progress, target results, skipped-file scope, and docs.
- Placeholder scan: No task depends on unnamed future behavior; each feature has concrete files, types, and test commands.
- Type consistency: Runtime signature data lives in `copy.rs`; `TransferPlan` remains copy intent; verification consumes `CompletedTransfer`; UI rows represent transfers using `Txx`.
