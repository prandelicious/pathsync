# Pathsync Run Layout and Summary Design

## Goal

Improve the interactive run layout and end-of-run summary so `pathsync` clearly shows copy activity while the run is active, then clearly proves per-target verification when the run completes.

The live view is movement-first. The summary is verification-first.

## Decisions

- Use one shared visual language for live and final views: a wide left work pane plus a narrow right `Run` box.
- In live TTY mode, keep workers primary and add a compact per-target strip below them.
- In the final summary, make target verification the first detailed section.
- Show failures inline with target status, then list exact failure rows nearby.
- Use `VERIFIED`, `ATTENTION`, and `FAILED` as final outcome language.
- Keep `Copied` and `Verified` separate in the run metrics.
- Use rich Unicode glyphs by default in TTY mode, with an ASCII fallback.
- Use structured line-oriented output for non-TTY/plain output instead of the rich dashboard.
- On narrow terminals, use a stacked fallback instead of compressing the dashboard.

## Live Wide TTY Layout

The wide live layout keeps the right `Run` box stable while the left pane shows active work.

```text
Pathsync (vlog)                                                   LIVE / COPY
────────────────────────────────────────────────────────────────────────────────────────────────────────
Copying  [██████████                    ]  58.2 GB of 133.0 GB   ETA 8m46s       ┌ Run ─────────────────────┐
overall  copying large files                                                       │ Scanned    2,941         │
                                                                                   │ Planned    318           │
Workers                                                                            │ Copied     141           │
⠋ T01  hashing    [██████████        ]  A001_C014_0101AB.MP4      8.2 GB   78.4 MB/s   T7      │ Verified   141           │
⠙ T02  copying    [████████          ]  A001_C015_0101AB.MP4      7.9 GB   64.0 MB/s   Archive │ Failed     1             │
⠹ T03  verifying  [██                ]  GX010193.MP4              2.1 GB   41.8 MB/s   T7      │ Bytes      58.2 / 133 GB │
  T04             [                  ]  idle                         --          --   --       │ Rate       142.4 MB/s    │
                                                                                   │ Elapsed    7m08s         │
Targets                                                                            │ ETA        8m46s         │
T7        [█████████████                 ]  31.0 / 66.5 GB   78.4 MB/s             │ Targets    2             │
Archive   [███████████                   ]  27.2 / 66.5 GB   64.0 MB/s             └──────────────────────────┘
────────────────────────────────────────────────────────────────────────────────────────────────────────
```

Worker rows should show, in order:

- spinner
- worker id
- phase: `hashing`, `copying`, `verifying`, or blank for idle
- per-file progress bar
- filename
- total file size
- current rate
- target label as its own final column

The target strip should show per-target byte progress and rate. Exact copied and verified counts belong in the final summary.

## Run Box

The right `Run` box is the stable ledger. It should contain only core metrics:

```text
┌ Run ─────────────────────┐
│ Scanned    2,941         │
│ Planned    318           │
│ Copied     141           │
│ Verified   141           │
│ Failed     1             │
│ Bytes      58.2 / 133 GB │
│ Rate       142.4 MB/s    │
│ Elapsed    7m08s         │
│ ETA        8m46s         │
│ Targets    2             │
└──────────────────────────┘
```

Worker counts, phase, and debug configuration should stay out of the stable box unless a future verbose/debug mode adds them.

## Final Summary Layout

The final summary should lead with target verification, not copied-file browsing.

```text
Pathsync (vlog)                                                   ATTENTION
────────────────────────────────────────────────────────────────────────────────────────────────────────
Verified [███████████████████████████---]  129.5 GB verified of 131.6 GB          ┌ Run ─────────────────────┐
                                                                                   │ Scanned    2,941         │
Target Results                                                                     │ Planned    318           │
Target      Planned   Copied   Verified   Copy Fail   Verify Fail   Result        │ Copied     316           │
T7              159      159        159           0             0    verified      │ Verified   314           │
Archive         159      157        155           1             2    attention     │ Failed     3             │
                                                                                   │ Bytes      129.5 / 131 GB│
Failures                                                                           │ Rate       121.7 MB/s    │
Target    Phase    File                    Error                                   │ Elapsed    18m01s        │
Archive   copy     GX010194.MP4            permission denied                       │ ETA        --            │
Archive   verify   GX010193.MP4            signature mismatch                      │ Targets    2             │
                                                                                   └──────────────────────────┘
Breakdown
Bucket             Files        Bytes        Share
copied large         204      128.4 GB       97.6%
copied small         112        3.2 GB        2.4%
skipped existing   2,623          0 B          --

Copied file preview
showing 20 of 316 copied files
────────────────────────────────────────────────────────────────────────────────────────────────────────
```

Target result rows should include:

- target
- planned
- copied
- verified
- copy fail
- verify fail
- result: `verified` or `attention`

Failure rows should include:

- target
- phase
- file
- error

Long failure lists should be capped with a clear `showing N of M failures` line.

## Outcome Language

Use these top-level outcomes:

- `VERIFIED`: every planned target copy was copied and verified.
- `ATTENTION`: the run completed but one or more target rows has copy or verification failures.
- `FAILED`: a systemic or hard copy failure prevented reliable completion.

Avoid using `warning` for verification failures because it understates the trust boundary.

## Narrow TTY Fallback

Below the wide threshold, drop the right `Run` box and target strip first. Keep workers readable in a stacked layout rather than compressing every region into a cramped dashboard.

The fallback should preserve:

- overall progress
- summary count line
- worker phase
- filename
- size
- target

## Plain Output

Non-TTY output should remain structured and line-oriented. It should not emit repeated dashboard frames.

Plain progress should include:

```text
copying files | 141/318 files | 4 active | 58.2 GB / 133.0 GB | rate 142.4 MB/s | eta 8m46s
target T7 | 31.0 GB / 66.5 GB | rate 78.4 MB/s | active 2
target Archive | 27.2 GB / 66.5 GB | rate 64.0 MB/s | active 1
```

Plain final output should include the same target verification facts as the TTY summary, but as grep-friendly rows or tables.

## Implementation Constraints

Use terminal width as an input to the renderer. The wide dashboard should only render when the left pane and right `Run` box can both preserve readable worker rows. When the terminal is too narrow, switch to the stacked fallback instead of truncating filenames or target labels into unreadable fragments.

The exact wide threshold can be implementation-defined, but it should be covered by tests at representative widths.

## Overflow

The live view should use fixed visible rows plus overflow counts:

- show a bounded number of worker rows based on configured parallelism and available height
- show a bounded number of target rows
- add `... N more targets` when target rows overflow
- show all target results in the final summary

Rows should not reorder during the run unless a future attention override is designed explicitly.

## Glyph Policy

TTY output should use Unicode by default:

- box drawing for borders
- solid block bars
- braille spinner frames

ASCII fallback should use the same layout model with simple glyph replacements.

## Test Expectations

Implementation should update or add tests for:

- wide live layout with worker rows, target strip, and `Run` box
- narrow stacked fallback
- final summary with `VERIFIED`, `ATTENTION`, and `FAILED`
- target verification first in the summary
- failure table containing target, phase, file, and error
- non-TTY structured progress lines
- Unicode and ASCII glyph rendering if the glyph policy becomes configurable

## Open Non-Goals

- Do not add a separate detailed report file in this layout pass.
- Do not make copied-file preview exhaustive in terminal output.
- Do not add row reordering for slow or problematic targets yet.
- Do not expose worker counts in the stable `Run` box.
