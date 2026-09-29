# Decision Log

Append-only log of non-obvious decisions and their rationale.

### [2026-07-03] D-1: Imported inputs are read-only; every stage writes a new copy

**Context**: A stage once edited its input in place.

**Decision**: Inputs are read-only after import; each stage writes its own output directory.

**Alternatives considered**:
- Edit in place with backups — backups drift

**Rationale**: A rerun must see the same inputs.

**Consequences**: Disk use grows with each stage.

**Tags**: inputs, layout

### [2026-07-05] D-2: Pin the pipeline environment

**Decision**: Pin every stage's environment by lockfile.

**Tags**: environment

### D-31 — Stage 02 runs once per batch (2026-07-15)

**Date:** 2026-07-15

**Decision:** Run stage 02 once per batch.

### D-31 addendum (2026-07-16): the batch list comes from the manifest
The manifest is the single source of the batch list.

### D-38
**Split stage 03 into a counting pass and a separate filtering pass that can be resumed on its own.**

**Date:** 2026-07-18 · **Supersedes:** the single-pass plan in F-004.

**Problem.** One pass could not be resumed.

### D-38 — Pin the stage 03 filter thresholds (2026-07-19)

**Date**: 2026-07-19 · **Status**: DECIDED (user)

**Decision.** Thresholds come from the pilot and are pinned in config.

### D-40 — Group stage 06 by label (2026-08-30) — ⛔ SUPERSEDED BY D-41 (2026-09-02)

**Decision.** Stage 06 groups by the label column.

### D-41 — Group stage 06 by measured similarity (2026-09-02)

**Date**: 2026-09-02 · **Status**: DECIDED (user, 2026-09-02) · **Tags**: stage-06, grouping, F-191

**Context.** D-40 grouped by label; F-191 shows the labels were wrong.

**Decision.** Group by measured similarity.

**Why.** Labels change; similarity is measured.

**Supersedes D-40.**

## D-108 — Stage 06 refuses inputs that fail the checksum (2026-09-01)

**Decision.** The check runs before any grouping.

## D-109

**Decision.** Stage 07 writes its merge log next to its output.

### D-157 — Stage 08 reads its thresholds from config (2026-09-28)

**Date**: 2026-09-28 · **Status**: DECIDED (agent, within D-41; flagged to the user) · **Tags**: stage-08, config, F-190

**Context.** The thresholds were written into the code.

**Decision.** Stage 08 reads them from config instead. Whether to keep the old defaults is a user decision.

**Why.** Config is reviewed with each run; code defaults are not.
