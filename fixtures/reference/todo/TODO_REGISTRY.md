# TODO Registry

All future work items for this project are tracked here.

## Status Key

| Status | Meaning |
|--------|---------|
| `open` | Not yet started |
| `blocked` | Waiting on something external |
| `complete` | Done (kept for reference) |

## Priority Key

| Priority | Meaning |
|----------|---------|
| `critical` | Blocks progress |
| `low` | Nice to have |

## Registry

| Item | Priority | Status | Category | Date | Author | File |
|------|----------|--------|----------|------|--------|------|
| **Summary figures for the stage 08 report**, made once from the final tables: one set of display names shared by every report (F-190). | medium | open | writing | 2026-09-25 | agent | D-157; F-190; `reports/figures/PLAN.md` |
| Fix `a` and `b` in the stage 02 config | high | `blocked` | pipeline | 2026-08-01 | agent | [stage02-config.md](stage02-config.md) |
| Replace the stage 01 panel gene | medium | done 2026-09-23 (applied with the rebuild) | pipeline | 2026-09-20 | agent | F-001; `lib/panel.py` |
| Characterize the sample-g data | low | **on-hold** (user, 2026-07-06) | data | 2026-07-03 | user | sample-g-notes.md |
| Third-pass review | high | **deferred (by design)** | analysis | 2026-07-30 | agent | `.living/decisions.md` D-41 |
| Audit the whole chain | high | recurring | process | 2026-07-18 | agent | C-1 |
| Build the stage 04 reference | critical | in-progress | pipeline | 2026-08-20 | agent | — |
| Re-run stage 05 on sample-d | medium | **done** | pipeline | 2026-09-19 | agent | F-020 |
| Characterize the sample-g data | low | open | data | 2026-09-01 | agent | — |

<!-- Add new entries above this line. -->

## #50 — Accept several input lists in stage 04 (F-004) — ✅ DONE 2026-08-03
**Opened**: 2026-08-01 · **Closed**: 2026-08-03

Stage 04 takes one input; it needs a list.

## #52 — Stage 05 lacks a smoke input — HALF DONE 2026-08-03
**Opened**: 2026-08-01 · **Priority**: medium-high

The stage 05 half is written; the stage 06 half is not.

| input | rows | share |
|---|---|---|
| covered | 5,000 | 60% |

| Add a self-test to stage 06 | high | open | testing | 2026-08-02 | agent | — |

## Move the manifest check into stage 01 (D-31 / F-004)
**Opened**: 2026-08-01 · **Priority**: low

Do this whenever stage 01 is next re-run.

## T-GroupCheck — report how stable each group is across reruns (a report, never a rename)
**Raised**: 2026-09-22 · **Stage**: 08 · **Status**: OPEN · **Decision**: D-157

Rerun stage 06 three times and count the inputs that change group.
**Tags**: stage-08, groups

## T-07Linkage — stage 07's manifest order (F-190): sort first?

**Status**: open, user's call · **Added**: 2026-09-28 · **Tags**: stage-07, manifest

Proposed fix: sort each manifest before reading it.

## T-Rebuild — rebuild the stage 03 index
**Status**: DONE 2026-09-14 as a streaming build (F-004)

Rebuilt.
