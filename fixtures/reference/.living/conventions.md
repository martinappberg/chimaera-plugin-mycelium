# Repo-Specific Conventions

Overrides to mycelium defaults for this project.

## C-1: Heavy files live on shared storage

Only text lives in the repository. See D-1.

## C-2 — Smoke-test before submitting a batch job (2026-07-10)

**Status**: active

Run one input end to end with `scripts/smoke.sh` before the full batch.

### C-2 addendum — the smoke run uses the production config

Otherwise it tests a different pipeline.

## Thresholds are derived from the data, then locked by the user

Derive, show the figures, and only then lock (F-003).
