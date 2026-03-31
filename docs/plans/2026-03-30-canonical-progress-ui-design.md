# Canonical 80-Column Progress UI Design

**Status:** Approved for implementation

**Goal:** Make the live TTY and post-run screens match the approved mockup exactly at 80 columns.

## Contract

- Treat the mockup as the canonical source of truth.
- Target exactly 80 columns for the rendered screen.
- Keep non-TTY output readable, but the parity requirement applies to the TTY renderer and the final TTY-style report.
- Preserve current copy semantics, best-effort failure handling, and adaptive scheduling behavior.

## Why The Current UI Drifts

- The live screen is composed from independent `indicatif` bars, not from a single canonical screen model.
- The post-run report is a generic summary made from sections like `Summary`, `Counts`, and `Buckets`, which do not match the mockup.
- The UI model only tracks a small set of progress fields and cannot render scanned totals, explicit failure counts, category rows, skip statistics, or error rows in the mockup format.
- Planning drops skipped-file information before the copy renderer ever runs, so the report cannot show skip categories faithfully.

## Recommended Architecture

Use a full-screen text composer for the canonical 80-column layout.

### Data Flow

1. Planning phase produces both transfer plans and planning stats.
2. Copy execution updates a runtime aggregation model from worker events.
3. Pure view-model builders convert planning stats plus runtime stats into:
   - `LiveScreenModel`
   - `PostRunScreenModel`
4. Pure formatting functions render those models into exact 80-column lines.
5. The TTY runtime redraws the full frame from those lines.

### Module Responsibilities

- `src/plan.rs`
  - Preserve scan and skip information instead of returning only `Vec<TransferPlan>`.
- `src/lib.rs`
  - Pass planning stats into copy execution.
- `src/progress_model.rs`
  - Define canonical screen models, worker-row models, category rows, and error rows.
- `src/progress_format.rs`
  - Own 80-column formatting, alignment, bar glyph rules, dividers, and exact line composition.
- `src/copy.rs`
  - Aggregate runtime state, throttle redraws, build screen models, and render full frames.

## Rendering Strategy

- Stop treating live rows as independent progress bars.
- Render the screen as a full frame of plain strings.
- Keep the canonical width fixed at 80 columns.
- Redraw the whole frame on phase changes, worker changes, progress updates, and final completion.
- Use `indicatif` only if it remains useful for terminal control, not for layout ownership. The layout must belong to the formatter.

## Canonical Live Screen

The live screen must include:

- Title row: `Pathsync (<job>)` on the left and `LIVE / COPY-*` on the right
- Full-width divider
- Summary stats block matching the mockup fields and order
- Labeled overall progress bar row
- Worker section header row
- One canonical worker row per visible worker slot

Each worker row must include:

- Worker tag: `W01`, `W02`, ...
- Fixed-width progress bar
- Current item text
- Right-aligned size and elapsed/rate columns where required by the mockup
- Stable idle rendering for unused worker slots

## Canonical Post-Run Screen

The post-run report must include:

- Title row: `Pathsync (<job>)` on the left and `COMPLETE` or `COMPLETE WITH ERRORS` on the right
- Full-width divider
- Summary stats block matching the mockup
- `Copy completion` bar row
- `By Category` section with canonical rows
- `Errors` section in canonical order and wording

## Required New Data

The renderer needs structured inputs that do not exist today:

- Scanned file count
- Planned file count
- Copied file count
- Failed file count
- Bytes scanned or planned as appropriate for the chosen line wording
- Bytes copied
- Skip counts by category
- Success counts and bytes by output category
- Failure counts by classification or operation bucket
- Ordered error preview rows

## Testing Strategy

- Add pure formatter tests that assert exact 80-column lines.
- Add snapshot-style tests for:
  - Live screen in active large-file mode
  - Live screen with idle worker rows
  - Post-run success
  - Post-run completion with errors
- Keep integration tests for semantic behavior, but add exact layout tests at the formatter layer so parity is enforceable.

## Risks And Constraints

- The fixed-width contract means long paths must be truncated deterministically.
- Preserving skipped-file stats requires a planning API change, not only a renderer change.
- The live redraw path must avoid flicker and must not break plain output mode.

## Acceptance Criteria

- At 80 columns, the live TTY screen matches the approved mockup structure exactly.
- At 80 columns, the post-run screen matches the approved mockup structure exactly.
- The renderer uses a canonical full-frame composer, not independent bar layout primitives.
- Planning and runtime stats provide enough data to fill every mockup field without placeholder gaps.
