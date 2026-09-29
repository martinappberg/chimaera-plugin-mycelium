---
topic: sample-cohort
description: What the sample cohort shows once through the pipeline.
last_updated: 2026-09-28
---

# Sample cohort

## F-170 (SUPERSEDED by F-177 — the reference build was already converted): Sample-f inputs use the old reference build
**Status:** established from the provider manifest.
**Date**: 2026-08-31 · **Stage**: 06 · **Tags**: reference, provider

**Result.** The provider manifest lists the old build for every sample-f input.

## F-177 — Sample-f inputs are already on the current reference build (2026-09-03)

**Status:** established by an independent rerun. **This CORRECTS F-170.**

**Setup.** Reran one sample-f input with `scripts/rerun_one.sh` (job 52000111) and compared coordinates.

**Result.** Coordinates match the current build; see `outputs/stage06/rerun_check.tsv` and `outputs/stage06/coords.png`.

**Why it matters.** The conversion step planned in D-31 is not needed.

## F-190 — Stage 07 counts inputs listed twice in the manifest twice (2026-09-26)

**Date**: 2026-09-26 · **Stage**: 07 · **Status**: finding; the fix is the user's call · **Tags**: stage-07, manifest,
duplicates, F-177

**Setup.** Scanned every manifest stage 07 v2 read (`~/r07_manifest_scan.py`, commit 1a2b3c4d).

**Result.** 12 of 40 manifests list an input twice, and stage 07 counts both.

**Consequence.** Put to the user with the stage 08 thresholds (F-177): one rerun from stage 07.

### Not yet decided — options, for the user

1. Deduplicate manifests in stage 07.
2. Keep them and flag repeated inputs.

## F-191 — Stage 06 groups are suspect after the label fix (2026-09-25)

> ⚠️ **SUSPECT as of 2026-09-25 — do not cite until re-derived.** The groups were built on the old labels. D-40, which rested on this, is superseded by D-41.

**Tags**: stage-06, labels

## F-192 — F-190 RETRACTED: the manifest scan read the wrong config (2026-09-27)

**Date**: 2026-09-27 · **Supersedes F-190's scope.** The scan read the v1 manifests.

**Result.** In the v2 manifests only 2 inputs repeat; see 07_manifests/v2/scan.tsv.
