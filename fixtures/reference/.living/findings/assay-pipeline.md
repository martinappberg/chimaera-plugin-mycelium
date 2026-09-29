---
topic: assay-pipeline
description: How the assay pipeline behaves across batches and stages.
created: 2026-07-01
last_updated: 2026-08-02
status: active
---

# Assay pipeline

## F-001: Batch 2 raises the background rate in pipeline stage 01
**Status:** supported
**Claim:** Samples processed in batch 2 show a ~3% higher background rate than batch 1 after stage 01 filtering.
**Implications:** Correct for batch before comparing background-based filters.
**Tags:** batch, background, stage-01

### Evidence Ledger
| Date | Run/Session | Dataset | Project | Result | Direction |
|------|-------------|---------|---------|--------|-----------|
| 2026-07-02 | 2026-07-02-001 | sample-a | pipeline | +3.1% in batch 2 | supports |
| 2026-07-09 | 2026-07-09-002 | sample-b | pipeline | +2.8% in batch 2 | supports |

### Open Questions
- Does the shift persist after the stage 02 re-filter?

## F-002: Stage 01 drops reads with ambiguous tags
**Status:** preliminary
**Claim:** Reads whose tag is ambiguous at one position are dropped by stage 01.
**Tags:** stage-01, reads

### Evidence Ledger
| Date | Run/Session | Dataset | Project | Result | Direction |
|---|---|---|---|---|---|
| 2026-07-04 | 2026-07-04-001 | sample-a | pipeline | 1.2% of reads dropped | supports |

### F-002 addendum: the drop is by design
The stage 01 notes say ambiguous tags are dropped; see `pipeline/stage01/NOTES.md`.

## F-003: The stage 02 merge step keeps sample labels
**Status:** supported (verified 2026-07-12 against `outputs/stage02/labels.tsv`)
**Claim:** Merging in stage 02 preserves every sample label.

### F-003 CORRECTION (2026-07-14): two labels were renamed, not kept
The merge renames `sample-c` to `sample-c1` when a duplicate exists. Regenerate from `scripts/check_labels.py`.

### F-003 RESOLVED (2026-07-15) — the rename is reverted in stage 02 v2
Job 51000123 re-ran stage 02 with the fix.

## F-004: Stage 03 wall time tracks read count, not sample count
**Status:** robust
**Claim:** Stage 03 wall time tracks total reads per input file.

### F-004 update: measured on the full batch
Measured on all 40 inputs (job 51000456).

### F-004 reprocess round 1 (2026-07-20): 36/40 inputs done
Four inputs failed on memory; re-queued with 64G.

#### F-004 addendum(2): the four re-queued inputs finished
All 40 done.

## F-005: Stage 04 covers the reference panel
**Status:** supported

### F-005 second run agrees with the first
The second run matched the first on coverage.

## F-020: Sample-d passes the stage 05 checks
**Status:** supported

## F-021: Sample-e passes the stage 05 checks
**Status:** preliminary

### F-020/F-021 addendum: both pass on the long-read path too
Checked on the long-read path.
