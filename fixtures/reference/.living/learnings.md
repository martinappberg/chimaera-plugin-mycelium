# Learnings

Append-only log of gotchas, surprises, and insights.

### [2026-07-04] The stage 01 filter drops genes silently

**Category**: gotcha

**What happened**: `stage01.filter` removed 120 genes before the marker step.

**Why it matters**: Marker panels lose genes without a warning.

**Resolution**: Filter after subsetting the panel.

**Tags**: [stage-01, filtering]

### [2026-09-27] A progress bar that counts files ends before the last file is written

**Category**: agent-process / tooling

**Symptom**: The bar read 100% while the last two files were still open.

**Why it matters**: The next step read half-written files.

**Generalisable rule**: Wait for each writer to close, not for a file count.

**Tags**: progress, files,
writers, mitigation_type=process

### L — A file's name is not its version

**What happened**: Two files named `v2` held different versions.

**How to apply**: Record a checksum beside every output, and compare it, never the name.

### 2026-07-30 — Fixing the shared helper fixed three callers at once

**Category**: insight

**What happened**: Three call sites had the same bug; see C-2.
