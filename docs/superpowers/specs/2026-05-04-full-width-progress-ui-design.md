# Pathsync Full-Width Progress UI Design

## Goal

Replace the live and final side-panel layouts with a full-width terminal UI. The new layout should use the available terminal width for the run facts, active transfers, target progress, and final verification tables without a narrow right `Run` box.

The design keeps the current formatter-first architecture. It does not require Ratatui.

## Problem

The current wide layout splits the screen into a large left work pane and a fixed-width right `Run` box. On wide terminals, the right box looks too narrow for the available space and makes the layout feel unbalanced. Long values also have less room than the terminal can provide.

The issue is visual proportion and information placement, not the lack of a TUI framework.

## Decisions

- Remove the side panel from live and final rich TTY output.
- Put run metrics in two compact full-width header rows.
- Keep active transfers as the main live section.
- Keep per-target live progress below active transfers.
- Keep final output verification-first: target results, failures, breakdown, copied preview.
- Keep non-TTY output structured and line-oriented.
- Keep Unicode glyphs by default with the existing ASCII fallback.
- Keep terminal width as a renderer input.

## Live Rich TTY Layout

The live screen should start with the job and status header, then show full-width run metrics before the active work.

```text
Pathsync (vlog)                                                                 COPYING
----------------------------------------------------------------------------------------
58.2 / 133.0 GB   44%   142.4 MB/s   ETA 8m46s   copied 141   verified 141
scanned 2,941     planned 318         failed 1     elapsed 7m08s   targets 2

Copying large files
[█████████████-----------------]  58.2 GB of 133.0 GB

Active transfers
T01  copying    A001_C014_0101AB.MP4          8.2 GB   78.4 MB/s   T7
T02  copying    A001_C015_0101AB.MP4          7.9 GB   64.0 MB/s   Archive
T03  verifying  GX010193.MP4                  2.1 GB   41.8 MB/s   T7
T04             idle                          --       --          --

Targets
T7        [██████████████----------------]  31.0 / 66.5 GB   78.4 MB/s   2 active
Archive   [████████████------------------]  27.2 / 66.5 GB   64.0 MB/s   1 active
----------------------------------------------------------------------------------------
```

The header metrics should favor scan speed over bookkeeping completeness. They should answer:

- how much data is done
- how far through the run we are
- current rate
- ETA
- copied and verified counts
- planned, failed, elapsed, and target count

The live section should not show a separate boxed ledger.

## Final Rich TTY Layout

The final screen should use the same full-width metric style, then lead with verification.

```text
Pathsync (vlog)                                                                 ATTENTION
----------------------------------------------------------------------------------------
129.5 / 131.6 GB verified   98%   copied 316   verified 314   failed 3
scanned 2,941               planned 318        elapsed 18m01s targets 2

Target results
Target       Planned   Copied   Verified   Copy fail   Verify fail   Result
T7               159      159        159           0             0    verified
Archive          159      157        155           1             2    attention

Failures
Target     Phase     File                    Error
Archive    copy      GX010194.MP4            permission denied
Archive    verify    GX010193.MP4            signature mismatch

Breakdown
Bucket                Files        Bytes       Share
copied large            204     128.4 GB       97.6%
copied small            112       3.2 GB        2.4%
skipped existing      2,623          0 B          --

Copied file preview
showing 20 of 316 copied files
----------------------------------------------------------------------------------------
```

Final output should keep the existing outcome language:

- `VERIFIED`: every planned target copy was copied and verified.
- `ATTENTION`: the run completed, but one or more target rows has copy or verification failures.
- `FAILED`: a systemic failure prevented reliable completion.

## Responsive Behavior

At wide widths, the renderer should allocate extra space to filenames and target labels instead of creating a side panel. At narrower widths, it should keep the current stacked fallback principle: preserve readable rows and truncate the least important fields first.

The implementation should define representative width tests, including:

- canonical 80-column output
- a wide terminal that previously used the side panel
- long file names
- long target labels
- ASCII glyph fallback

## Implementation Notes

This design should be implemented in the existing formatter/model boundary:

- `src/progress_model.rs` continues to own display-ready state.
- `src/progress_format.rs` owns terminal layout and width decisions.
- `src/copy.rs` should not learn layout details.

The change should remove or replace the wide side-panel renderer path. It should not introduce Ratatui unless a future feature adds interactive job browsing, dry-run filtering, or keyboard navigation.

## Non-Goals

- Do not add interactive controls.
- Do not add a job browser.
- Do not introduce Ratatui in this pass.
- Do not change non-TTY output unless needed to keep terminology consistent.
- Do not add a separate report file.
