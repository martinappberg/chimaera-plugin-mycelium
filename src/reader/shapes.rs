//! The shapes real Mycelium projects write, one test each, and the
//! synthetic reference tree (`fixtures/reference/`) end to end. Content is
//! invented; the shapes are cut from a real three-month project.
// Tuples of what each entry reads as keep the expectations legible.
#![allow(clippy::type_complexity)]

use super::tests::{read, Fixture};
use super::*;
use chimaera_plugin_api::serde_json;

fn one_finding(label: &str, body: &str) -> (Knowledge, Finding) {
    let fx = Fixture::new(label);
    fx.write(".living/findings/t.md", body);
    let k = read(fx.root());
    let f = k.topics[0].findings[0].clone();
    (k, f)
}

fn refs(r: &[Ref]) -> Vec<&str> {
    r.iter().map(|r| r.id.as_str()).collect()
}

fn cites(c: &[Cite]) -> Vec<String> {
    c.iter().map(|c| format!("{}:{}", c.kind, c.text)).collect()
}

#[test]
fn a_template_finding_keeps_its_claim_statement_and_its_status_as_written() {
    let (_, f) = one_finding(
        "template",
        "## F-007: Stage 02 keeps labels\n\
         **Status:** supported\n\
         **Claim:** Merging in stage 02 preserves every sample label.\n\
         **Implications:** Downstream joins can trust the label column.\n\
         **Tags:** stage-02, labels\n\n\
         ### Evidence Ledger\n\
         | Date | Run/Session | Dataset | Project | Result | Direction |\n\
         |---|---|---|---|---|---|\n\
         | 2026-07-02 | 2026-07-02-001 | sample-a | pipeline | all kept | supports |\n\n\
         ### Open Questions\n- Does it hold for sample-b?\n",
    );
    assert_eq!(f.claim, "Stage 02 keeps labels");
    assert_eq!(
        f.statement,
        "Merging in stage 02 preserves every sample label."
    );
    assert_eq!(
        (f.status.as_str(), f.stated.as_str()),
        ("supported", "supported")
    );
    assert_eq!(f.key, "t/F-007");
    assert_eq!(f.date, "");
    assert_eq!(
        f.span,
        Some(Span {
            path: ".living/findings/t.md".into(),
            line: 1,
            end_line: 13
        })
    );
    assert_eq!(f.ledger.len(), 1);
    assert_eq!(f.questions, ["Does it hold for sample-b?"]);
}

#[test]
fn a_prose_finding_reads_its_date_and_the_evidence_it_names_inline() {
    let (k, f) = one_finding(
        "prose",
        "## F-228 — Stage 03 runtime grows linearly with input size (2026-09-28)\n\n\
         **Setup.** Timed stage 03 at five input sizes, from `03_runs/v2/rows.tsv` \
         (job 51973032), per D-151 and C-12.\n\n\
         **Result.** No knee; see `~/r03_timing.py` and figure `out/runtime.png`.\n\n\
         **Why it matters.** Batch size can be set by budget (F-227).\n\n\
         Regenerate from `scripts/timing.py`; commit 1a2b3c4d.\n",
    );
    assert_eq!(f.claim, "Stage 03 runtime grows linearly with input size");
    assert_eq!(f.date, "2026-09-28");
    assert_eq!((f.status.as_str(), f.stated.as_str()), ("unknown", ""));
    assert_eq!(refs(&f.refs), ["D-151", "C-12", "F-227"]);
    assert_eq!(
        cites(&f.cites),
        [
            "job:51973032",
            "data:03_runs/v2/rows.tsv",
            "script:~/r03_timing.py",
            "figure:out/runtime.png",
            // Jobs and commits are read first on their line.
            "commit:1a2b3c4d",
            "script:scripts/timing.py"
        ]
    );
    assert!(k.warnings.is_empty(), "{:?}", k.warnings);
}

#[test]
fn an_off_vocabulary_status_is_stated_verbatim_and_its_correction_lands_on_the_target() {
    let fx = Fixture::new("established");
    fx.write(
        ".living/findings/t.md",
        "## F-171: Inputs use the old build\n**Status:** preliminary\n\n\
         ## F-178: Inputs are on the current build\n\
         **Status:** established by an independent rerun. **This CORRECTS F-171.**\n\n\
         ## F-179: Throughput\n**Status:** supported (measured 2026-09-18 from `rows.tsv.gz`)\n",
    );
    let k = read(fx.root());
    let f = &k.topics[0].findings;
    assert_eq!(f[1].status, "unknown");
    assert_eq!(
        f[1].stated,
        "established by an independent rerun. This CORRECTS F-171."
    );
    assert_eq!(
        f[1].amends,
        [Amend {
            kind: "corrects",
            id: "F-171".into()
        }]
    );
    assert_eq!(
        f[0].state,
        Some(State {
            kind: "corrected",
            by: Some("F-178".into())
        })
    );
    // The status is the Mycelium word the Status starts with; the rest is
    // stated, not dropped.
    assert_eq!(f[2].status, "supported");
    assert_eq!(
        f[2].stated,
        "supported (measured 2026-09-18 from rows.tsv.gz)"
    );
}

#[test]
fn fields_joined_by_a_middle_dot_are_separate_fields() {
    let (_, f) = one_finding(
        "inline",
        "## F-200 — Stage 06 excludes two inputs\n\n\
         **Date**: 2026-09-05 · **Stage**: 07 · **Status**: resolved · **Tags**: stage-06, inputs\n\n\
         **Result.** Two inputs fail the check.\n",
    );
    assert_eq!(f.date, "2026-09-05");
    assert_eq!(
        (f.status.as_str(), f.stated.as_str()),
        ("unknown", "resolved")
    );
    assert_eq!(f.tags, ["stage-06", "inputs"]);

    // A decision's line, with the value inside the bold and ids in the tags.
    let fx = Fixture::new("inline-decision");
    fx.write(
        ".living/decisions.md",
        "### D-157 — Stage 08 reads its thresholds (2026-09-28)\n\n\
         **Date**: 2026-09-28 · **Status**: DECIDED (agent, within D-152's limits) · **Tags**: aim1, 08, F-231\n",
    );
    let d = &read(fx.root()).decisions[0];
    assert_eq!(d.stated, "DECIDED (agent, within D-152's limits)");
    assert_eq!(d.tags, ["aim1", "08", "F-231"]);
    assert_eq!(refs(&d.refs), ["D-152", "F-231"]);
    let (_, f) = one_finding(
        "in-bold",
        "## F-12: Kept, labelled\n**Status: open, on purpose.** Kept for now.\n",
    );
    assert_eq!(f.stated, "open, on purpose. Kept for now.");
}

#[test]
fn tags_that_wrap_onto_the_next_line_are_all_read() {
    let (_, f) = one_finding(
        "wrapped-tags",
        "## F-074: Coverage per region\n**Status:** supported\n\
         **Tags:** f-074, coverage, region, cells-per-sample,\n\
         sample-a, sample-b, d-35\n\nSome prose after the tags.\n",
    );
    assert_eq!(
        f.tags,
        [
            "f-074",
            "coverage",
            "region",
            "cells-per-sample",
            "sample-a",
            "sample-b",
            "d-35"
        ]
    );
}

#[test]
fn follow_up_headings_thread_under_their_finding_with_a_kind() {
    let fx = Fixture::new("follow-ups");
    fx.write(
        ".living/findings/t.md",
        "## F-024: Two inputs share one output dir\n**Status:** supported\n\n\
         ### F-024 RESOLVED\nRe-keyed.\n\n\
         ## F-026: A glob drops half the reads\nText.\n\n\
         ### F-026 VALIDATED + reprocess launched (2026-07-15)\nLaunched.\n\n\
         ### F-026 reprocess round 1 (2026-07-15): 29/37 done\nMost done.\n\n\
         ### F-026 RESOLVED (2026-07-15): all 37 units reprocessed\nDone.\n\n\
         ## F-037: The barcode join is broken\nText.\n\n\
         ### F-037 RESOLUTION: root cause pinned\nPinned.\n\n\
         ## F-073: Stage 04 stalls\nText.\n\n\
         ### F-073 CORRECTION (2026-07-23) — the true root cause\nScratch space.\n\n\
         ### F-073 addendum (2026-07-23) — unblocked\nRuns again.\n\n\
         ## F-172: Three technologies\nText.\n\n\
         ### F-172 CORRECTION\nThere are three.\n\n\
         ## F-054: A two-epoch control\nText.\n\n\
         ### F-054 addendum — the control agrees\nAgrees.\n\n\
         ### F-054 addendum(2): and again\nAgain.\n",
    );
    let k = read(fx.root());
    let threads: Vec<(String, Vec<(&str, String, String)>, Option<&str>)> = k.topics[0]
        .findings
        .iter()
        .map(|f| {
            (
                f.id.clone(),
                f.addenda
                    .iter()
                    .map(|a| (a.kind, a.label.clone(), a.date.clone()))
                    .collect(),
                f.state.as_ref().map(|s| s.kind),
            )
        })
        .collect();
    let s = |kind: &'static str, label: &str, date: &str| (kind, label.to_owned(), date.to_owned());
    assert_eq!(
        threads,
        [
            (
                "F-024".to_owned(),
                vec![s("resolution", "RESOLVED", "")],
                Some("resolved")
            ),
            (
                "F-026".to_owned(),
                vec![
                    s("update", "Update", "2026-07-15"),
                    s("update", "Reprocess round 1 (2026-07-15)", "2026-07-15"),
                    s("resolution", "RESOLVED (2026-07-15)", "2026-07-15"),
                ],
                Some("resolved")
            ),
            (
                "F-037".to_owned(),
                vec![s("resolution", "RESOLUTION", "")],
                Some("resolved")
            ),
            (
                "F-054".to_owned(),
                vec![
                    s("addendum", "Addendum", ""),
                    s("addendum", "Addendum (2)", "")
                ],
                None
            ),
            (
                "F-073".to_owned(),
                vec![
                    s("correction", "CORRECTION (2026-07-23)", "2026-07-23"),
                    s("addendum", "Addendum (2026-07-23)", "2026-07-23"),
                ],
                // The newest follow-up is an addendum, not a resolution.
                None
            ),
            (
                "F-172".to_owned(),
                vec![s("correction", "CORRECTION", "")],
                None
            ),
        ]
    );
    let f26 = &k.topics[0].findings[1];
    assert_eq!(
        f26.addenda[0].title,
        "VALIDATED + reprocess launched (2026-07-15)"
    );
    assert_eq!(f26.addenda[2].title, "all 37 units reprocessed");
    assert_eq!(
        f26.addenda[2].span.as_ref().map(|s| (s.line, s.end_line)),
        Some((
            line_of(&fx, "### F-026 RESOLVED"),
            line_of(&fx, "### F-026 RESOLVED") + 1
        ))
    );
    // Follow-ups are no findings of their own, and no id collides.
    assert_eq!(k.counts.findings, 6);
    assert!(k.tidy.is_empty(), "{:?}", k.tidy);
}

fn line_of(fx: &Fixture, start: &str) -> u32 {
    let text = std::fs::read_to_string(fx.root().join(".living/findings/t.md")).unwrap();
    let at = text.split('\n').position(|l| l.starts_with(start)).unwrap();
    line_no(at)
}

#[test]
fn a_joint_follow_up_belongs_to_its_first_id() {
    let fx = Fixture::new("joint");
    fx.write(
        ".living/findings/t.md",
        "## F-020: Sample-d passes\n**Status:** supported\n\n\
         ## F-021: Sample-e passes\n**Status:** preliminary\n\n\
         ### F-020/F-021 addendum: both pass on the long-read path\nChecked.\n",
    );
    let k = read(fx.root());
    let f = &k.topics[0].findings;
    assert_eq!(f.len(), 2);
    assert_eq!(f[0].addenda.len(), 1);
    assert_eq!(f[0].addenda[0].title, "both pass on the long-read path");
    assert_eq!(refs(&f[0].refs), ["F-021"]);
    assert!(f[1].addenda.is_empty());
}

#[test]
fn markers_the_agent_wrote_are_states() {
    let fx = Fixture::new("markers");
    fx.write(
        ".living/findings/t.md",
        "## F-171 (SUPERSEDED by F-177 — the collection is converted): Inputs use the old build\n\
         **Status:** established\n\n\
         ## F-177: Inputs are on the current build\n**Status:** established\n\n\
         ## F-184 — Stage 06 groups are stable across reruns\n\n\
         > ⚠️ **SUSPECT as of 2026-09-02 — do not cite.** D-40, which relied on this, is superseded by D-41.\n\n\
         ## F-037: The join is broken\nText.\n\n\
         ## F-041 — F-037 RETRACTED: the input was truncated\n\
         **Date:** 2026-07-18 · **Supersedes F-037 and revises D-38.**\n",
    );
    fx.write(
        ".living/decisions.md",
        "### D-104 — Keep the old index (2026-08-31) — ⛔ SUPERSEDED BY D-110 (2026-09-02)\n\
         **Decision.** Keep it.\n\n\
         ### D-89 — Use both sources — ⛔ SCOPE CLAUSE SUPERSEDED BY D-116 (2026-09-08)\n\
         **Decision.** Both.\n\n\
         ### D-110 — Rebuild the index (2026-09-02)\n**Supersedes D-104.**\n",
    );
    let k = read(fx.root());
    let state = |id: &str| {
        k.topics[0]
            .findings
            .iter()
            .find(|f| f.id == id)
            .and_then(|f| f.state.clone())
            .map(|s| (s.kind, s.by))
    };
    assert_eq!(state("F-171"), Some(("superseded", Some("F-177".into()))));
    assert_eq!(state("F-184"), Some(("suspect", None)));
    // F-041 both supersedes and retracts F-037: the stronger word stands.
    assert_eq!(state("F-037"), Some(("retracted", Some("F-041".into()))));
    assert_eq!(state("F-041"), None);
    let d = |id: &str| {
        let d = k.decisions.iter().find(|d| d.id == id).unwrap();
        (
            d.title.clone(),
            d.date.clone(),
            d.state.clone().map(|s| (s.kind, s.by)),
        )
    };
    assert_eq!(
        d("D-104"),
        (
            "Keep the old index — ⛔ SUPERSEDED BY D-110 (2026-09-02)".to_owned(),
            "2026-08-31".to_owned(),
            Some(("superseded", Some("D-110".into())))
        )
    );
    // A clause going is not the entry going.
    assert_eq!(d("D-89").2, None);
}

#[test]
fn reused_finding_ids_get_distinct_keys_and_one_tidy_row() {
    let fx = Fixture::new("collisions");
    fx.write(
        ".living/findings/a.md",
        "## F-129 — One claim\nText.\n\n## F-129 — Another claim\nText.\n",
    )
    .write(
        ".living/findings/b.md",
        "## F-129: A third claim\nText.\n\n## F-079: Fine\n",
    )
    .write(".living/findings/c.md", "## F-079: Unrelated\n");
    let k = read(fx.root());
    let keys: Vec<&str> = k
        .topics
        .iter()
        .flat_map(|t| &t.findings)
        .map(|f| f.key.as_str())
        .collect();
    assert_eq!(
        keys,
        ["a/F-129", "a/F-129~2", "b/F-079", "b/F-129", "c/F-079"]
    );
    assert!(k.warnings.is_empty(), "{:?}", k.warnings);
    assert_eq!(k.tidy.len(), 1);
    let row = &k.tidy[0];
    assert_eq!(row.kind, "duplicate-id");
    assert_eq!(
        row.text,
        "2 finding ids each name more than one finding: F-079, F-129."
    );
    assert_eq!(refs(&row.refs), ["F-079", "F-129"]);
    assert!(row.ask.contains(
        "F-129 (.living/findings/a.md:1, .living/findings/a.md:4, .living/findings/b.md:1)"
    ));
}

#[test]
fn decisions_in_both_eras_read_their_ids_dates_and_fields() {
    let fx = Fixture::new("decision-eras");
    fx.write(
        ".living/decisions.md",
        "# Decision Log\n\n\
         ### [2026-07-03] D-1: Inputs are read-only — every stage writes a new copy\n\n\
         **Context**: A stage once edited its input.\n\n**Decision**: Inputs are read-only.\n\n\
         **Alternatives considered**: edit in place with backups — backups drift\n\n\
         **Rationale**: A rerun must see the same inputs.\n\n**Consequences**: Disk use grows per stage.\n\n\
         **Tags**: inputs, layout\n\n\
         ### D-157 — Stage 08 reads its thresholds (2026-09-28)\n\n\
         **Date**: 2026-09-28 · **Status**: DECIDED (user) · **Tags**: stage-08\n\n\
         **Context.** The thresholds were in code.\n\n**Decision.** Read them from config.\n\n\
         **Why.** Config is reviewed with each run.\n\n\
         ### D-47 (2026-07-24) — One report suite\n**Decision:** One suite.\n\n\
         ### D-52\n**Date:** 2026-07-26\n**Decision:** **Option B only — option A is dropped.**\n\n\
         ### D-38\n**Split the counting pass from the filtering pass so each can be resumed.**\n\n\
         ### D-38 — REVISION (2026-07-18, same day)\nRevised.\n\n\
         ### [2026-07-20] D-38: A second D-38\n**Decision**: Unrelated.\n\n\
         ### [2026-07-21] D3: A number without a dash\n\n\
         ## D-108 — Stage 06 refuses unchecked inputs (2026-09-01)\n**Decision.** Refuse it.\n\n\
         ## D-109\n**Decision.** Stage 07 writes its log beside its output.\n\n\
         ## Archive\n\nNotes, not an entry.\n",
    );
    let k = read(fx.root());
    let got: Vec<(&str, &str, &str)> = k
        .decisions
        .iter()
        .map(|d| (d.id.as_str(), d.date.as_str(), d.title.as_str()))
        .collect();
    assert_eq!(
        got,
        [
            ("D-157", "2026-09-28", "Stage 08 reads its thresholds"),
            ("D-108", "2026-09-01", "Stage 06 refuses unchecked inputs"),
            ("D-52", "2026-07-26", "Option B only — option A is dropped."),
            ("D-47", "2026-07-24", "One report suite"),
            ("D-3", "2026-07-21", "A number without a dash"),
            ("D-38", "2026-07-20", "A second D-38"),
            (
                "D-1",
                "2026-07-03",
                "Inputs are read-only — every stage writes a new copy"
            ),
            // Undated last, the later in the file first.
            ("D-109", "", "Stage 07 writes its log beside its output."),
            (
                "D-38",
                "",
                "Split the counting pass from the filtering pass so each can be resumed."
            ),
        ]
    );
    let d157 = &k.decisions[0];
    assert_eq!(d157.stated, "DECIDED (user)");
    assert_eq!(d157.context, "The thresholds were in code.");
    assert_eq!(d157.decision, "Read them from config.");
    assert_eq!(d157.rationale, "Config is reviewed with each run.");
    assert_eq!(d157.tags, ["stage-08"]);
    assert_eq!(
        k.decisions[6].alternatives,
        ["edit in place with backups — backups drift"]
    );
    // The revision is part of the first D-38: its span runs over it.
    let d38 = &k.decisions[8];
    assert_eq!(
        d38.span.as_ref().map(|s| (s.line, s.end_line)),
        Some((34, 38))
    );
    // The fingerprint is 0.1.3's, from the heading as written then.
    assert_eq!(
        d157.fp,
        fingerprint(
            "decision",
            "",
            "D-157 — Stage 08 reads its thresholds (2026-09-28)"
        )
    );
    // `## D-108` / `## D-109` are entries, with a warning and a Tidy up row;
    // the reused D-38 is another row.
    assert_eq!(
        k.warnings,
        [".living/decisions.md: 2 entries use ## headings (mycelium 0.7 expects ###)"]
    );
    let kinds: Vec<(&str, Vec<&str>)> = k.tidy.iter().map(|t| (t.kind, refs(&t.refs))).collect();
    assert_eq!(
        kinds,
        [
            ("duplicate-id", vec!["D-38"]),
            ("off-index", vec!["D-108", "D-109"])
        ]
    );

    // With no explicit id anywhere, mycelium's positional ids.
    let fx = Fixture::new("positional");
    fx.write(
        ".living/learnings.md",
        "### [2026-07-01] First\n**What happened**: a\n\n### [2026-07-02] Second\n\n## [2026-07-03] Mislevelled\n",
    );
    let positional = read(fx.root());
    let ids: Vec<(&str, &str)> = positional
        .learnings
        .iter()
        .map(|l| (l.id.as_str(), l.title.as_str()))
        .collect();
    assert_eq!(
        ids,
        [("", "Mislevelled"), ("L-2", "Second"), ("L-1", "First")]
    );
}

#[test]
fn learnings_read_the_labels_agents_use() {
    let fx = Fixture::new("learnings");
    fx.write(
        ".living/learnings.md",
        "### [2026-09-27] A progress bar that counts files ends early\n\n\
         **Category**: agent-process / tooling\n\n\
         **Symptom**: It read 100% while files were still open.\n\n\
         **Why it matters**: The next step read half-written files.\n\n\
         **Generalisable rule**: Wait for every writer to close.\n\n\
         **Tags**: progress, files,\nwriters, mitigation_type=process\n\n\
         ### L — A file's name is not its version\n\n\
         **What happened**: Two files named v2 held different versions.\n\n\
         **How to apply**: Compare checksums, never names.\n\n\
         ### 2026-07-30 — Fixing the shared helper fixed three callers\n\n**Category**: insight\n",
    );
    let k = read(fx.root());
    let l = &k.learnings;
    assert_eq!(l.len(), 3);
    assert_eq!(l[0].title, "A progress bar that counts files ends early");
    assert_eq!(l[0].category, "other");
    assert_eq!(l[0].what, "It read 100% while files were still open.");
    assert_eq!(l[0].why, "The next step read half-written files.");
    assert_eq!(l[0].resolution, "Wait for every writer to close.");
    assert_eq!(l[0].tags, ["progress", "files", "writers"]);
    assert_eq!(
        (l[1].date.as_str(), l[1].category.as_str()),
        ("2026-07-30", "insight")
    );
    assert_eq!(l[2].title, "A file's name is not its version");
    assert_eq!(l[2].resolution, "Compare checksums, never names.");
    let ids: Vec<&str> = l.iter().map(|l| l.id.as_str()).collect();
    assert_eq!(ids, ["L-1", "L-3", "L-2"]);
}

#[test]
fn todo_sections_and_rows_appended_below_them_are_todos() {
    let fx = Fixture::new("todo-shapes");
    fx.write("MYCELIUM.md", "").write(
        "todo/TODO_REGISTRY.md",
        "# TODO Registry\n\n## Status Key\n\n| Status | Meaning |\n|---|---|\n| `open` | Not started |\n\n\
         ## Registry\n\n\
         | Item | Priority | Status | Category | Date | Author | File |\n\
         |---|---|---|---|---|---|---|\n\
         | **Shared display names for the reports**: one table for every stage. | medium | open | data | 2026-09-25 | agent | D-117; F-198; `notes/aim3 plan.md` Aim 3 |\n\
         | Fix `a` and `b` in stage 02 | high | `blocked` | pipeline | 2026-08-01 | agent | [fix.md](fix.md) |\n\
         | Replace the panel gene | medium | done 2026-09-23 (applied with the rebuild) | pipeline | 2026-09-20 | agent | F-221 |\n\
         | Onboard sample-g | low | **on-hold** (user, 2026-07-06) | data | 2026-07-03 | user | sample-g.md |\n\
         | Third-pass review | high | **deferred (by design)** | analysis | 2026-07-30 | agent | — |\n\
         | Wont fix this | low | won't do — superseded | data | 2026-07-30 | agent | — |\n\n\
         <!-- Add new entries above this line -->\n\n\
         ## #50 — Several input lists (F-132) — ✅ DONE 2026-08-03\n\
         **Opened**: 2026-08-01 · **Closed**: 2026-08-03\n\nStage 04 takes one list.\n\n\
         ## #52 — No smoke input — HALF DONE 2026-08-03\n**Priority**: medium\n\nHalf written.\n\n\
         | region | cells | share |\n|---|---|---|\n| covered | 5,000 | 60% |\n\n\
         | Appended by upsert | high | open | testing | 2026-08-02 | agent | — |\n\n\
         ## Move the manifest check into stage 01 (D-83 / F-126)\n**Opened**: 2026-08-01 · **Priority**: low\n\nWhenever.\n\n\
         ## T-GroupTiers — are the two tiers one group?\n\
         **Raised**: 2026-09-28 · **Status**: PROPOSED, awaiting the user · **Decision**: —\n\nOptions.\n\n\
         ## T-Classifier — use a published classifier\n**Status**: DONE 2026-09-14 as trained models (F-209)\n",
    );
    let k = read(fx.root());
    let got: Vec<(&str, &str, &str, bool, &str)> = k
        .todos
        .iter()
        .map(|t| {
            (
                t.key.as_str(),
                t.source,
                t.title.as_str(),
                t.closed,
                t.file.as_str(),
            )
        })
        .collect();
    assert_eq!(
        got,
        [
            (
                "todo/r1",
                "table",
                "Shared display names for the reports",
                false,
                ""
            ),
            (
                "todo/r2",
                "table",
                "Fix a and b in stage 02",
                false,
                "todo/fix.md"
            ),
            ("todo/r3", "table", "Replace the panel gene", true, ""),
            (
                "todo/r4",
                "table",
                "Onboard sample-g",
                false,
                "todo/sample-g.md"
            ),
            ("todo/r5", "table", "Third-pass review", false, ""),
            ("todo/r6", "table", "Wont fix this", true, ""),
            ("todo/r7", "table", "Appended by upsert", false, ""),
            (
                "todo/#50",
                "section",
                "Several input lists (F-132) — ✅ DONE 2026-08-03",
                true,
                ""
            ),
            (
                "todo/#52",
                "section",
                "No smoke input — HALF DONE 2026-08-03",
                false,
                ""
            ),
            (
                "todo/s3",
                "section",
                "Move the manifest check into stage 01 (D-83 / F-126)",
                false,
                ""
            ),
            (
                "todo/T-GroupTiers",
                "section",
                "are the two tiers one group?",
                false,
                ""
            ),
            (
                "todo/T-Classifier",
                "section",
                "use a published classifier",
                true,
                ""
            ),
        ]
    );
    let r1 = &k.todos[0];
    assert_eq!(refs(&r1.refs), ["D-117", "F-198"]);
    // A code span inside a cell keeps its backticks; a cell that is one
    // code span is unwrapped.
    assert_eq!(k.todos[1].item, "Fix `a` and `b` in stage 02");
    assert_eq!(k.todos[1].status, "blocked");
    assert_eq!(k.todos[3].status, "on-hold (user, 2026-07-06)");
    let s52 = &k.todos[8];
    assert_eq!((s52.priority.as_str(), s52.date.as_str()), ("medium", ""));
    // #52's span stops before the appended row, which is the table's.
    assert_eq!(
        s52.span.as_ref().map(|s| (s.line, s.end_line)),
        Some((27, 34))
    );
    assert_eq!(
        k.todos[6].span.as_ref().map(|s| (s.line, s.end_line)),
        Some((36, 36))
    );
    assert_eq!(k.todos[10].status, "proposed, awaiting the user");
    assert_eq!(
        k.todos[7].item,
        "Several input lists (F-132) — ✅ DONE 2026-08-03\n\nStage 04 takes one list."
    );
    assert_eq!(k.counts.todos, 8);
    let off: Vec<(&str, Vec<&str>)> = k.tidy.iter().map(|t| (t.kind, refs(&t.refs))).collect();
    assert_eq!(off, [("off-index", vec!["T-GroupTiers", "T-Classifier"])]);
    assert!(k.tidy[0].text.starts_with(
        "5 to-dos are kept as ## sections below the registry table in todo/TODO_REGISTRY.md"
    ));
}

#[test]
fn closed_is_a_closed_word_leading_the_status() {
    for closed in [
        "complete",
        "done",
        "**done**",
        "done 2026-09-23 (applied with the rebuild)",
        "DONE 2026-09-14 as trained models",
        "won't do",
        "wont-do — superseded by #61",
        "cancelled",
        "resolved",
    ] {
        assert!(is_closed(closed), "{closed}");
    }
    for open in [
        "open",
        "in-progress",
        "blocked",
        "recurring",
        "**on-hold** (user, 2026-07-06)",
        "**deferred (by design)**",
        "half done",
        "not done",
        "doneness check",
        "",
    ] {
        assert!(!is_closed(open), "{open}");
    }
    assert!(is_closed("✅ done") && is_closed("✓ complete") && is_closed("✔"));
    assert!(heading_says_closed("#50 — x — ✅ DONE 2026-08-03"));
    assert!(heading_says_closed("#60 — atlas labels — COMPLETE"));
    assert!(!heading_says_closed("#52 — x — HALF DONE 2026-08-03"));
    assert!(!heading_says_closed("Make sure it is done right"));
}

#[test]
fn the_newest_handoff_wins_and_numbered_headings_fill_the_slots() {
    let fx = Fixture::new("handoffs");
    fx.write(".living/decisions.md", "")
        .write(
            ".mycelium/last-session.md",
            "## What was worked on\n\
             - Completed the session work recorded in the finalized session log.\n\
             - Modified `config.yaml`\n\n\
             ## Next steps\n\
             - Review the finalized session log and continue from the current branch state.\n",
        )
        .write(
            ".mycelium/run/claude/sess-a/last-session.md",
            "# Session handoff\n\n## 1. Goal\nRebuild the report.\n\n## 2. Done\n- Design review.\n\n\
             ## 3. In flight\n- Full run.\n\n## 4. Next\n1. Read the result.\n2. Freeze.\n\n\
             ## 5. Rules\nNo edits while jobs run.\n\n\
             ## PARKED USER DECISION — keep the old defaults?\nNot yet decided.\n",
        )
        .write(
            ".mycelium/run/codex/sess-b/last-session.md",
            "## What was worked on\n- An older session.\n",
        );
    // The shared stub at 12:09, the hand-written run handoff at 18:29, an
    // older one weeks before.
    fx.set_mtime_at(".mycelium/last-session.md", 1_790_597_340);
    fx.set_mtime_at(".mycelium/run/claude/sess-a/last-session.md", 1_790_620_140);
    fx.set_mtime_at(".mycelium/run/codex/sess-b/last-session.md", 1_788_861_600);
    let k = read(fx.root());
    let left = k.left_off.as_ref().unwrap();
    assert_eq!(left.path, ".mycelium/run/claude/sess-a/last-session.md");
    assert_eq!(left.session_id.as_deref(), Some("sess-a"));
    assert_eq!(left.current, "Rebuild the report.\n\n- Full run.");
    assert_eq!(left.worked_on, "- Design review.");
    assert_eq!(left.next, ["Read the result.", "Freeze."]);
    assert_eq!(
        left.span,
        Some(Span {
            path: left.path.clone(),
            line: 1,
            end_line: 20
        })
    );
    let sources: Vec<(&str, u64)> = left
        .sources
        .iter()
        .map(|s| (s.path.as_str(), s.written_ms / 1000))
        .collect();
    assert_eq!(
        sources,
        [
            (".mycelium/run/claude/sess-a/last-session.md", 1_790_620_140),
            (".mycelium/last-session.md", 1_790_597_340),
            (".mycelium/run/codex/sess-b/last-session.md", 1_788_861_600),
        ]
    );
    let asks: Vec<(&str, &str, u32)> = k
        .asks
        .iter()
        .map(|a| (a.text.as_str(), a.date.as_str(), a.span.line))
        .collect();
    assert_eq!(
        asks,
        [
            (
                "PARKED USER DECISION — keep the old defaults?",
                "2026-09-28",
                19
            ),
            ("Not yet decided.", "2026-09-28", 20),
        ]
    );
    assert_eq!(k.tidy.len(), 1);
    assert_eq!(k.tidy[0].kind, "handoff-stub");
    assert_eq!(
        k.tidy[0].text,
        "The shared handoff .mycelium/last-session.md is the Stop hook's fallback stub, while a newer hand-written handoff exists at .mycelium/run/claude/sess-a/last-session.md."
    );

    // Newest wins even when it is the stub; the tidy row still says so.
    fx.set_mtime_at(".mycelium/last-session.md", 1_790_630_000);
    let k = read(fx.root());
    assert_eq!(k.left_off.unwrap().path, ".mycelium/last-session.md");
    assert!(k.tidy[0].text.ends_with(
        "while a hand-written handoff exists at .mycelium/run/claude/sess-a/last-session.md."
    ));
}

#[test]
fn asks_come_from_the_handoff_and_recent_entries_only() {
    let fx = Fixture::new("asks");
    fx.write(
        ".living/findings/t.md",
        "## F-100 — An old question (2026-08-01)\n\nPut to the user: whether to rerun.\n\n\
         ## F-227 — Stage 08 thresholds (2026-09-26)\n\n\
         **Consequence.** Put to the user with the stage 07 counts (F-226): one rerun from `stage07`.\n\n\
         ### Not yet decided — options, for the user\n\n1. Rerun.\n2. Keep.\n",
    )
    .write(
        ".living/decisions.md",
        "### D-157 — Thresholds come from config (2026-09-28)\n\n**Decision.** Whether to keep the old defaults is a user decision.\n",
    );
    let k = read(fx.root());
    let asks: Vec<(&str, &str, &str, (u32, u32))> = k
        .asks
        .iter()
        .map(|a| {
            (
                a.text.as_str(),
                a.date.as_str(),
                a.source.key.as_str(),
                (a.span.line, a.span.end_line),
            )
        })
        .collect();
    let fp = k.decisions[0].fp.as_str();
    assert_eq!(
        asks,
        [
            (
                "Whether to keep the old defaults is a user decision.",
                "2026-09-28",
                fp,
                (3, 3)
            ),
            (
                "Put to the user with the stage 07 counts (F-226): one rerun from stage07.",
                "2026-09-26",
                "t/F-227",
                (7, 7)
            ),
            (
                "Not yet decided — options, for the user",
                "2026-09-26",
                "t/F-227",
                (9, 9)
            ),
        ]
    );
}

#[test]
fn conventions_and_sessions_are_read() {
    let fx = Fixture::new("conventions");
    fx.write(
        ".living/conventions.md",
        "# Conventions\n\nIntro.\n\n## C-12 — Verify the input first\n\n**Status**: active\n\nSee F-038.\n\n\
         ### C-12 addendum\nMore.\n\n## A section without an id\n\nText citing `scripts/check.py`.\n",
    )
    .write(
        ".living/generated-conventions/validate-inputs/convention.md",
        "---\nid: validate-inputs\ntitle: Validate inputs before outputs.\nstatus: proposed\n---\n\n## Statement\n\nCheck them.\n",
    )
    .write(
        ".living/log/LOG_REGISTRY.md",
        "# Session Log Registry\n\n\
         | Date | Session ID | Project | Branch | Duration | Files Changed | Summary | Key Outputs | Status | Tags | Log |\n\
         |---|---|---|---|---|---|---|---|---|---|---|\n\
         | 2026-07-03 | 2026-07-03-001 | p | main | 40m | 12 | Set up. | config | complete | setup | [log](2026-07-03-001-p.md) |\n\
         | 2026-09-28 | 2026-09-28-001 | p | v3 | 48m | 5 | Planned v3. | | complete | | [log](2026-09-28-001-p.md) |\n",
    );
    let k = read(fx.root());
    let conventions: Vec<(&str, &str, &str, &str, (u32, u32))> = k
        .conventions
        .iter()
        .map(|c| {
            (
                c.key.as_str(),
                c.id.as_str(),
                c.title.as_str(),
                c.status.as_str(),
                (c.span.line, c.span.end_line),
            )
        })
        .collect();
    assert_eq!(
        conventions,
        [
            (
                "conventions/C-12",
                "C-12",
                "Verify the input first",
                "active",
                (5, 12)
            ),
            (
                "conventions/s2",
                "",
                "A section without an id",
                "",
                (14, 16)
            ),
            (
                "generated-conventions/validate-inputs",
                "validate-inputs",
                "Validate inputs before outputs.",
                "proposed",
                (1, 9)
            ),
        ]
    );
    assert_eq!(refs(&k.conventions[0].refs), ["F-038"]);
    assert_eq!(cites(&k.conventions[1].cites), ["script:scripts/check.py"]);
    let sessions: Vec<(&str, &str, &str, &str)> = k
        .sessions
        .iter()
        .map(|s| {
            (
                s.id.as_str(),
                s.branch.as_str(),
                s.summary.as_str(),
                s.log.as_str(),
            )
        })
        .collect();
    assert_eq!(
        sessions,
        [
            (
                "2026-09-28-001",
                "v3",
                "Planned v3.",
                ".living/log/2026-09-28-001-p.md"
            ),
            (
                "2026-07-03-001",
                "main",
                "Set up.",
                ".living/log/2026-07-03-001-p.md"
            ),
        ]
    );
    assert_eq!((k.counts.conventions, k.counts.sessions), (3, 2));
}

/// Everything at once, over `fixtures/reference/` — the counts pinned.
#[test]
fn the_reference_tree_reads_every_shape() {
    let fx = Fixture::copy_of("reference", "reference");
    let run = ".mycelium/run/claude/7c1f0a52-0000-4000-8000-00000000000a/last-session.md";
    let older = ".mycelium/run/codex/3b2e9d10-0000-4000-8000-00000000000b/last-session.md";
    // 2026-09-28 18:29 and 12:09 UTC; 2026-09-08 10:00 UTC.
    fx.set_mtime_at(run, 1_790_620_140);
    fx.set_mtime_at(".mycelium/last-session.md", 1_790_597_340);
    fx.set_mtime_at(older, 1_788_861_600);
    let k = read(fx.root());

    assert_eq!(
        serde_json::to_value(&k.counts).unwrap(),
        serde_json::json!({
            "findings": 17, "decisions": 10, "learnings": 4, "open": 13,
            "todos": 12, "questions": 1, "conventions": 4, "sessions": 3
        })
    );
    assert_eq!(
        k.warnings,
        [".living/decisions.md: 2 entries use ## headings (mycelium 0.7 expects ###)"]
    );

    // Findings: every key unique, 13 distinct ids over 17 findings.
    let findings: Vec<&Finding> = k.topics.iter().flat_map(|t| &t.findings).collect();
    let keys: HashSet<&str> = findings.iter().map(|f| f.key.as_str()).collect();
    let ids: HashSet<&str> = findings.iter().map(|f| f.id.as_str()).collect();
    assert_eq!((findings.len(), keys.len(), ids.len()), (17, 17, 13));
    let by_key = |key: &str| *findings.iter().find(|f| f.key == key).unwrap();
    let states: Vec<(&str, &str, Option<&str>)> = findings
        .iter()
        .filter_map(|f| {
            f.state
                .as_ref()
                .map(|s| (f.key.as_str(), s.kind, s.by.as_deref()))
        })
        .collect();
    assert_eq!(
        states,
        [
            ("assay-pipeline/F-003", "resolved", None),
            ("sample-cohort/F-170", "superseded", Some("F-177")),
            ("sample-cohort/F-190", "retracted", Some("F-192")),
            ("sample-cohort/F-191", "suspect", None),
        ]
    );
    let kinds = |key: &str| -> Vec<&str> { by_key(key).addenda.iter().map(|a| a.kind).collect() };
    assert_eq!(kinds("assay-pipeline/F-003"), ["correction", "resolution"]);
    assert_eq!(
        kinds("assay-pipeline/F-004"),
        ["update", "update", "addendum"]
    );
    assert_eq!(kinds("assay-pipeline/F-005"), ["update"]);
    assert_eq!(kinds("assay-pipeline/F-020"), ["addendum"]);
    let f177 = by_key("sample-cohort/F-177");
    assert_eq!(
        (
            f177.status.as_str(),
            f177.stated.as_str(),
            f177.date.as_str()
        ),
        (
            "unknown",
            "established by an independent rerun. This CORRECTS F-170.",
            "2026-09-03"
        )
    );
    assert_eq!(
        cites(&f177.cites),
        [
            "job:52000111",
            "script:scripts/rerun_one.sh",
            "data:outputs/stage06/rerun_check.tsv",
            "figure:outputs/stage06/coords.png"
        ]
    );
    assert_eq!(
        by_key("reference-panel/F-201~2").claim,
        "Panel v3 keeps the control probes"
    );

    // Decisions: both eras, the follow-up folded, `##` entries kept.
    let decisions: Vec<(&str, &str)> = k
        .decisions
        .iter()
        .map(|d| (d.id.as_str(), d.date.as_str()))
        .collect();
    assert_eq!(
        decisions,
        [
            ("D-157", "2026-09-28"),
            ("D-41", "2026-09-02"),
            ("D-108", "2026-09-01"),
            ("D-40", "2026-08-30"),
            ("D-38", "2026-07-19"),
            ("D-38", "2026-07-18"),
            ("D-31", "2026-07-15"),
            ("D-2", "2026-07-05"),
            ("D-1", "2026-07-03"),
            ("D-109", ""),
        ]
    );

    // To-dos: 10 table rows (one appended below a section), 6 sections.
    let todos: Vec<(&str, bool)> = k.todos.iter().map(|t| (t.source, t.closed)).collect();
    assert_eq!(todos.iter().filter(|t| t.0 == "table").count(), 10);
    assert_eq!(todos.iter().filter(|t| t.0 == "section").count(), 6);
    let closed: Vec<&str> = k
        .todos
        .iter()
        .filter(|t| t.closed)
        .map(|t| t.key.as_str())
        .collect();
    assert_eq!(closed, ["todo/r3", "todo/r8", "todo/#50", "todo/T-Rebuild"]);

    // The newest handoff, every handoff listed.
    let left = k.left_off.as_ref().unwrap();
    assert_eq!(left.path, run);
    let sources: Vec<&str> = left.sources.iter().map(|s| s.path.as_str()).collect();
    assert_eq!(sources, [run, ".mycelium/last-session.md", older]);

    let asks: Vec<(&str, &str)> = k
        .asks
        .iter()
        .map(|a| (a.source.kind, a.source.id.as_str()))
        .collect();
    assert_eq!(
        asks,
        [
            ("handoff", "7c1f0a52-0000-4000-8000-00000000000a"),
            ("handoff", "7c1f0a52-0000-4000-8000-00000000000a"),
            ("decision", "D-157"),
            ("finding", "F-190"),
            ("finding", "F-190"),
            ("finding", "F-190"),
        ]
    );
    let tidy: Vec<&str> = k.tidy.iter().map(|t| t.kind).collect();
    assert_eq!(
        tidy,
        [
            "duplicate-id",
            "duplicate-id",
            "off-index",
            "off-index",
            "handoff-stub",
            "duplicate-todo"
        ]
    );
    let conventions: Vec<&str> = k.conventions.iter().map(|c| c.key.as_str()).collect();
    assert_eq!(
        conventions,
        [
            "conventions/C-1",
            "conventions/C-2",
            "conventions/s3",
            "generated-conventions/validate-inputs"
        ]
    );
    let sessions: Vec<&str> = k.sessions.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(
        sessions,
        ["2026-09-28-001", "2026-09-27-001", "2026-07-03-001"]
    );
    assert_eq!(
        k.guidance,
        [Guidance {
            path: "MYCELIUM.md",
            label: "MYCELIUM.md",
            description: "How agents record knowledge here"
        }]
    );

    // On the wire: spans say `end_line`, the constants are there.
    let json = serde_json::to_value(&k).unwrap();
    assert_eq!(
        json["topics"][0]["findings"][0]["span"],
        serde_json::json!({"path": ".living/findings/assay-pipeline.md", "line": 11, "end_line": 24})
    );
    assert_eq!(json["id_shapes"].as_array().unwrap().len(), 5);
    assert_eq!(
        json["id_shapes"][4]["pattern"],
        r"T-\d*[A-Za-z][A-Za-z0-9]*"
    );
    assert_eq!(json["labels"]["kinds"]["todo"], "to-do");
    assert_eq!(json["labels"]["sections"]["overview"], "Overview");
    let tones: Vec<&str> = json["labels"]["status_words"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["tone"].as_str().unwrap())
        .collect();
    assert!(tones
        .iter()
        .all(|t| ["neutral", "accent", "good", "warn", "bad"].contains(t)));
}

/// A 0.1.3-shaped input serializes what 0.1.3 did plus only what it newly
/// has: nothing empty rides the wire.
#[test]
fn new_fields_are_omitted_when_empty() {
    let fx = Fixture::new("omitted");
    fx.write(
        ".living/findings/t.md",
        "## F-001: A claim\n**Status:** supported\n",
    );
    let json = serde_json::to_value(read(fx.root())).unwrap();
    let f = json["topics"][0]["findings"][0].as_object().unwrap();
    let mut keys: Vec<&str> = f.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "claim",
            "id",
            "implications",
            "key",
            "ledger",
            "line",
            "questions",
            "span",
            "stated",
            "status",
            "tags",
            "updated"
        ]
    );
    let top = json.as_object().unwrap();
    for absent in [
        "conventions",
        "sessions",
        "asks",
        "tidy",
        "guidance",
        "left_off",
    ] {
        assert!(!top.contains_key(absent), "{absent}");
    }
    assert!(top.contains_key("id_shapes") && top.contains_key("labels"));
    assert!(json["topics"][0].get("date").is_none());
}

/// A project far past any real one — 1,000 findings, and near-cap
/// decisions and learnings files with long fields — reads in bounded time
/// and fits the host's snapshot cap, shortening only what the view can
/// read from the files.
#[test]
fn a_huge_project_fits_the_snapshot_budget() {
    let fx = Fixture::new("huge");
    let long = |seed: &str| format!("{seed} ").repeat(90);
    let finding = |n: usize| {
        format!(
            "## F-{n}: Claim number {n} about stage {n}\n**Status:** supported\n\
             **Claim:** {}\n**Implications:** {}\n**Tags:** a{n}, b{n}\n\n\
             See `out/stage{n}/table.tsv`, `scripts/s{n}.py`, D-{n} and C-{}.\n\n",
            long("claim"),
            long("because"),
            n % 999 + 1
        )
    };
    for topic in 0..10 {
        let body: String = (topic * 100 + 1..=topic * 100 + 100).map(finding).collect();
        fx.write(&format!(".living/findings/topic-{topic:02}.md"), &body);
    }
    let decisions: String = (1..=700)
        .map(|n| {
            format!(
                "### D-{n} — Decision {n} (2026-01-{:02})\n\n**Context.** {}\n\n**Decision.** {}\n\n**Why.** {}\n\n",
                n % 28 + 1,
                long("context"),
                long("decided"),
                long("reason")
            )
        })
        .collect();
    let learnings: String = (1..=700)
        .map(|n| {
            format!(
                "### [2026-02-{:02}] Learning {n}\n\n**What happened**: {}\n\n**Why it matters**: {}\n\n**Resolution**: {}\n\n",
                n % 28 + 1,
                long("happened"),
                long("matters"),
                long("resolved")
            )
        })
        .collect();
    assert!(decisions.len() < MAX_FILE_BYTES as usize && learnings.len() < MAX_FILE_BYTES as usize);
    fx.write(".living/decisions.md", &decisions)
        .write(".living/learnings.md", &learnings);
    let started = std::time::Instant::now();
    let k = read(fx.root());
    let took = started.elapsed();
    let json = serde_json::to_vec(&k).unwrap();
    eprintln!("huge: {} bytes in {took:?}; {:?}", json.len(), k.warnings);
    assert!(json.len() <= SNAPSHOT_BUDGET, "{} bytes", json.len());
    assert!(took.as_secs() < 10, "{took:?}");
    assert_eq!(k.counts.findings, 1000);
    assert!(
        k.warnings[0].contains("long text fields are shortened"),
        "{:?}",
        k.warnings
    );
    // Shortened, never dropped: every entry keeps its span to the full text.
    assert!(k
        .learnings
        .iter()
        .all(|l| l.what.len() <= SHORT_TEXT_BYTES && l.span.is_some()));
}

#[test]
fn over_budget_the_cheapest_loss_comes_first() {
    let fx = Fixture::copy_of("fit", "reference");
    let full = read(fx.root());
    let size = |k: &Knowledge| serde_json::to_vec(k).unwrap().len();
    // Shortening alone doesn't fit this budget; dropping cites and some of
    // the oldest entries does.
    let mut k = full.clone();
    let mut notes = Notes::default();
    let budget = size(&full) - 4000;
    fit(&mut k, budget, &mut notes);
    assert!(size(&k) <= budget);
    let warnings = notes.finish();
    assert_eq!(warnings.len(), 3, "{warnings:?}");
    assert!(warnings[2].starts_with("…and only the newest are shown"));
    assert!(k
        .topics
        .iter()
        .flat_map(|t| &t.findings)
        .all(|f| f.cites.is_empty()));
    let findings: usize = k.topics.iter().map(|t| t.findings.len()).sum();
    assert_eq!(k.counts.findings as usize, findings);
    assert_eq!(k.counts.decisions as usize, k.decisions.len());
    assert_eq!(k.counts.learnings as usize, k.learnings.len());

    // Under budget, nothing changes.
    let mut same = full.clone();
    let mut notes = Notes::default();
    fit(&mut same, size(&full), &mut notes);
    assert_eq!(size(&same), size(&full));
    assert!(notes.finish().is_empty());
}

/// When findings are the bulk, findings give way — not the decisions and
/// learnings beside them — and the budget holds whatever it takes.
#[test]
fn the_largest_section_gives_way_and_the_budget_always_holds() {
    let fx = Fixture::new("findings-heavy");
    let question = "why ".repeat(425);
    let findings: String = (1..=40)
        .map(|n| {
            let questions: String = (0..20).map(|q| format!("- {q} {question}\n")).collect();
            format!(
                "## F-{n}: Claim {n}\n**Status:** supported\n\n### Open Questions\n{questions}\n"
            )
        })
        .collect();
    fx.write(".living/findings/t.md", &findings).write(
        ".living/decisions.md",
        "### [2026-01-01] D-1: One\n**Decision**: a\n\n### [2026-01-02] D-2: Two\n**Decision**: b\n",
    );
    let full = read(fx.root());
    let size = |k: &Knowledge| serde_json::to_vec(k).unwrap().len();
    for budget in [size(&full) / 2, 60_000, 20_000] {
        let mut k = full.clone();
        let mut notes = Notes::default();
        fit(&mut k, budget, &mut notes);
        assert!(size(&k) <= budget, "{budget}: {}", size(&k));
        assert_eq!(k.decisions.len(), 2, "{budget}");
    }
}

#[test]
fn a_dashless_id_needs_a_separator() {
    assert_eq!(
        explicit_id("D1: Inputs are read-only", b'D'),
        Some(("D-1".to_owned(), "Inputs are read-only"))
    );
    assert_eq!(
        explicit_id("D12 — Pin it", b'D'),
        Some(("D-12".to_owned(), "Pin it"))
    );
    assert_eq!(
        explicit_id("D-157 Pin it", b'D').map(|(id, _)| id),
        Some("D-157".to_owned())
    );
    assert_eq!(explicit_id("D-38", b'D'), Some(("D-38".to_owned(), "")));
    assert_eq!(explicit_id("L2 cache misses slow stage 04", b'L'), None);
    assert_eq!(
        explicit_id("L1-norm penalty hides sparse features", b'L'),
        None
    );
    assert_eq!(explicit_id("L2.5 cutoff", b'L'), None);
    assert_eq!(
        explicit_id("D1. Pin it", b'D'),
        Some(("D-1".to_owned(), "Pin it"))
    );
    assert_eq!(explicit_id("L12345: too long", b'L'), None);
    assert_eq!(explicit_id("Decision 4", b'D'), None);
}

/// Token soup in every file the reader opens — multi-byte dashes and
/// marks, unbalanced bold and backticks, ids, pipes, headings — never
/// panics, and stays bounded.
#[test]
fn hostile_text_never_panics() {
    #[rustfmt::skip]
    const TOKENS: &[&str] = &[
        "## ", "### ", "#### ", "# ", "F-", "D-", "L-", "C-", "T-", "12", "007", "/F-", "**", "*",
        "`", "``", "·", "—", "–", "-", ":", ".", "(", ")", "[", "](", "|", "\\|", "⛔", "✅",
        "⚠️", "’", "é", "日本", "SUPERSEDED", "BY", "by", "RETRACTED", "SUSPECT", "corrects",
        "supersedes", "retracts", "addendum", "CORRECTION", "RESOLVED", "reprocess", "round",
        "Status", "Tags", "Date", "2026-09-28", "(2026-09-28)", "job", "commit", "a1b2c3d",
        "12345678", "x.py", "dir/", "Put to the user", "not yet decided", "DONE", "HALF",
        "```", "~~~", "<!--", "-->", "---", "1.", "- ", " ", "  ", "\n", "\n\n", "\t", "'s",
    ];
    let mut seed: u64 = 0x2545_f491_4f6c_dd1d;
    let mut soup = |n: usize| {
        let mut out = String::new();
        for _ in 0..n {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            out.push_str(TOKENS[(seed % TOKENS.len() as u64) as usize]);
        }
        out
    };
    for round in 0..8 {
        let fx = Fixture::new(&format!("soup-{round}"));
        for rel in [
            ".living/findings/a.md",
            ".living/findings/b.md",
            ".living/decisions.md",
            ".living/learnings.md",
            ".living/conventions.md",
            ".living/log/LOG_REGISTRY.md",
            ".living/generated-conventions/x/convention.md",
            "todo/TODO_REGISTRY.md",
            ".mycelium/last-session.md",
            ".mycelium/run/claude/s1/last-session.md",
        ] {
            fx.write(rel, &soup(4000));
        }
        // A registry header, so the soup is read as rows and sections too.
        let registry = format!(
            "| Item | Priority | Status | Category | Date | Author | File |\n|---|---|---|---|---|---|---|\n{}",
            soup(4000)
        );
        fx.write("todo/TODO_REGISTRY.md", &registry);
        let k = read(fx.root());
        let json = serde_json::to_vec(&k).unwrap();
        assert!(json.len() <= SNAPSHOT_BUDGET);
        assert!(k.warnings.len() <= MAX_WARNINGS + 1);
    }
}

#[test]
fn long_registries_keep_only_what_is_shown() {
    let fx = Fixture::new("long-registries");
    let rows: String = (1..=5000)
        .map(|n| format!("| Item {n} | low | open | x | 2026-01-01 | a | — |\n"))
        .collect();
    let sessions: String = (1..=1000)
        .map(|n| {
            format!(
                "| 2026-01-{:02} | s-{n:04} | p | main | 1m | 1 | Did {n}. | | complete | | |\n",
                n % 28 + 1
            )
        })
        .collect();
    fx.write("MYCELIUM.md", "")
        .write(
            "todo/TODO_REGISTRY.md",
            &format!(
                "| Item | Priority | Status | Category | Date | Author | File |\n|---|---|---|---|---|---|---|\n{rows}\n## #1 — A section\nText.\n"
            ),
        )
        .write(
            ".living/log/LOG_REGISTRY.md",
            &format!(
                "| Date | Session ID | Project | Branch | Duration | Files Changed | Summary | Key Outputs | Status | Tags | Log |\n|---|---|---|---|---|---|---|---|---|---|---|\n{sessions}"
            ),
        );
    let k = read(fx.root());
    assert_eq!(k.todos.len(), MAX_ENTRIES);
    assert_eq!(k.todos[MAX_ENTRIES - 1].title, "Item 1000");
    assert_eq!(k.sessions.len(), MAX_SESSIONS);
    // The newest: the last rows written, dated newest first.
    assert!(k.sessions.iter().all(|s| s.id.as_str() > "s-0600"));
    assert!(k
        .warnings
        .contains(&"todo/TODO_REGISTRY.md: showing the first 1000 of 5001 todos".to_owned()));
    assert!(k.warnings.contains(
        &".living/log/LOG_REGISTRY.md: showing the newest 400 of 1000 sessions".to_owned()
    ));
}

#[test]
fn positional_ids_count_the_lines_mycelium_counts() {
    // mycelium's collect_entries numbers every column-1 `### ` line, even
    // one inside a fence: so do positional ids.
    let fx = Fixture::new("positional-fenced");
    fx.write(
        ".living/learnings.md",
        "# Learnings\n\n```markdown\n### [YYYY-MM-DD] Title\n```\n\n\
         ### [2026-01-02] First\n**What happened**: a\n\n### [2026-01-03] Second\n",
    );
    let k = read(fx.root());
    let ids: Vec<(&str, &str)> = k
        .learnings
        .iter()
        .map(|l| (l.id.as_str(), l.title.as_str()))
        .collect();
    assert_eq!(ids, [("L-3", "Second"), ("L-2", "First")]);
}

#[test]
fn stated_quotes_the_status_that_status_came_from() {
    let (_, f) = one_finding(
        "stated-source",
        "## F-1: A claim\n**Status:** established by rerun\n\n\
         ### F-1 addendum: later\n**Status:** supported\n",
    );
    assert_eq!(
        (f.status.as_str(), f.stated.as_str()),
        ("supported", "supported")
    );
    let (_, f) = one_finding(
        "stated-own",
        "## F-1: A claim\n**Status:** preliminary (one run)\n\n\
         ### F-1 addendum: later\n**Status:** robust\n",
    );
    assert_eq!(
        (f.status.as_str(), f.stated.as_str()),
        ("preliminary", "preliminary (one run)")
    );
}

#[test]
fn a_span_ends_at_its_last_line_of_content() {
    let fx = Fixture::new("span-end");
    fx.write(
        ".living/decisions.md",
        "### [2026-01-01] A\n**Decision**: a\n\n---\n\n<!-- Add new entries above this line -->\n\n### [2026-01-02] B\n**Decision**: b\n",
    );
    let k = read(fx.root());
    let a = k.decisions.iter().find(|d| d.title == "A").unwrap();
    assert_eq!(a.span.as_ref().map(|s| (s.line, s.end_line)), Some((1, 2)));
}

/// One 2 MB line of `·`-joined bold fields, or of markers, reads in
/// bounded time.
#[test]
fn a_huge_line_of_fields_or_markers_stays_linear() {
    let fx = Fixture::new("huge-line");
    let fields = format!(
        "### D-1 t\n**Tags**: a {}\n",
        "· **status: x** ".repeat(60_000)
    );
    let markers = format!("### D-2 see D-3 {}\n", "RETRACTED ".repeat(80_000));
    fx.write(".living/decisions.md", &format!("{fields}\n{markers}"))
        .write(
            ".living/findings/t.md",
            &format!(
                "## F-1: x\n> ⚠️ {}\n",
                "SUSPECT superseded ".repeat(100_000)
            ),
        );
    let started = std::time::Instant::now();
    let k = read(fx.root());
    assert!(started.elapsed().as_secs() < 30, "{:?}", started.elapsed());
    // Both files were read (each under the per-file cap).
    assert_eq!(k.decisions.len(), 2, "{:?}", k.warnings);
    assert_eq!(k.counts.findings, 1);
}
