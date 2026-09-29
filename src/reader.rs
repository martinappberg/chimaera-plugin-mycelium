//! Read-only reader for a workspace's mycelium project knowledge — the
//! structured provider behind the Knowledge view (Chimaera's
//! docs/knowledge-redesign-plan.md, "Wire spec — what plugin 0.2.0 adds";
//! the plugin it runs in: docs/plugin-system-plan.md).
//!
//! - **Read-only, always.** Agents write knowledge through mycelium's skills
//!   and hooks; Chimaera only reads it. Nothing here creates, locks, or
//!   rewrites a file, and `.mycelium/locks` is never touched.
//! - **Report what was written; never rate.** A finding's `stated` is its
//!   Status as written, `status` the Mycelium word it starts with (else
//!   `unknown`); nothing here computes, maps or defaults a status. States
//!   (superseded, retracted, …) come only from markers the agent wrote.
//! - **mycelium 0.7.2 is the format, and what agents actually write is the
//!   input.** Findings (`.living/findings/<topic>.md`: `## F-NNN: claim` or
//!   `## F-NNN — title (date)`, `**Status:**`, an `### Evidence Ledger`,
//!   `### Open Questions`, prose led by `**Setup.**`-style labels, follow-up
//!   headings — `F-NNN addendum`, `CORRECTION`, `RESOLVED`, …), decisions and
//!   learnings (`### [YYYY-MM-DD] Title` or `### D-157 — title (date)` +
//!   `**Field**:` lines, `·`-joined inline fields), conventions,
//!   `todo/TODO_REGISTRY.md` (the table and the `##` to-do sections under
//!   it), the session log registry, and the newest `.mycelium` handoff.
//!   Malformed input degrades to fewer items plus a `warnings` line. It
//!   never errors and never panics.
//! - **Fence-aware.** Headings and fields inside ``` / ~~~ fences (the
//!   CommonMark rules of mycelium's `markdown_fences.py`) or HTML comments are
//!   content, not entries. mycelium's own `collect_entries` is not
//!   fence-aware; an example entry in a code block must not become knowledge.
//! - **Bounded.** A file over [`MAX_FILE_BYTES`] is skipped unread, and every
//!   collection and text field has a cap (constants below; the per-entry
//!   `refs`/`cites`/`amends` caps live in `scan.rs`). Symlinks are never
//!   followed, so the reader cannot leave the workspace. These budgets sit
//!   under the host's own (8 MiB a read, 4,096 entries a listing, a 4 MiB
//!   snapshot).
//! - **Through [`Fs`].** Every path is workspace-relative and answered by the
//!   host (or, in native tests, `std::fs` under the host's rules). [`plan`]
//!   is the metadata-only pass: its [`Plan::stamp`] lets a caller skip a
//!   re-parse ([`read`]) when nothing changed.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet, VecDeque};
use std::ops::Range;

use chimaera_plugin_api::Stat;
use serde::Serialize;

use crate::fs::{is_symlink_refusal, Fs, NOT_REGULAR};
use crate::scan::{
    ask_sentences, cap_chars, collapse_ws, date_of_ms, day_number, first_sentence, id_at,
    is_iso_date, own_state, paren_date, plain, scan_amends, scan_cites, scan_refs,
    scan_retracted_ids, strip_links, trailing_date, Amend, Cite, Ref, State,
};

/// A knowledge file over this size is skipped unread. mycelium's logs are
/// append-only prose (2 MiB is years of entries), so a bigger file is a
/// pasted dump, and parsing it would spend the login node's RSS budget.
const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;
/// Bytes read across all files by one [`read`]: 200 topic files at the
/// per-file cap would otherwise be 400 MiB of NFS I/O for one view. Topic
/// files are read last, so they are what a spent budget drops.
const MAX_TOTAL_BYTES: u64 = 16 * 1024 * 1024;
/// Topic files read from `.living/findings/`, first by slug.
const MAX_TOPIC_FILES: usize = 200;
/// Entry / `F-` headings examined per file. Only [`MAX_ENTRIES`] are ever
/// materialized, but a hostile file of bare headings would otherwise hold
/// hundreds of thousands of spans at once. Logs keep the LAST ones (they
/// append); topic files the first.
const MAX_SCANNED_ENTRIES: usize = 5000;
/// Directory entries examined per listing, so a runaway directory can't turn
/// a cheap [`plan`] into a crawl.
const MAX_DIR_ENTRIES: usize = 4096;
/// Items kept per kind (decisions, learnings, findings, todos, questions).
/// Decisions and learnings keep the newest. The host's 4 MiB snapshot cap is
/// the real bound; this one only stops a pathological file.
const MAX_ENTRIES: usize = 1000;
/// Conventions and sessions kept.
const MAX_CONVENTIONS: usize = 400;
const MAX_SESSIONS: usize = 400;
/// `.living/generated-conventions/<name>/` directories examined.
const MAX_GENERATED_CONVENTIONS: usize = 200;
/// Evidence rows kept per finding: the newest, since ledgers append.
const MAX_LEDGER_ROWS: usize = 50;
/// Ceiling for every text field, "…" included; cuts land on a char boundary.
const MAX_TEXT_BYTES: usize = 2 * 1024;
/// A Tidy up row's drafted request: long enough to name every file and id.
const MAX_ASK_BYTES: usize = 8 * 1024;
/// A to-do's title, in chars.
const MAX_TITLE_CHARS: usize = 160;
const MAX_TAGS: usize = 20;
const MAX_QUESTIONS_PER_FINDING: usize = 20;
/// Addenda kept per finding: the newest, since they append.
const MAX_ADDENDA: usize = 50;
/// Items kept per list (handoff blockers / next steps, a decision's
/// alternatives).
const MAX_LIST_ITEMS: usize = 50;
/// `.mycelium/run/<host>/<session>/` directories examined for handoffs.
const MAX_RUN_DIRS: usize = 256;
/// "Waiting on you" items kept, and candidates held per entry.
const MAX_ASKS: usize = 20;
const MAX_ASKS_PER_ENTRY: usize = 5;
/// Findings and decisions this many days older than the newest dated entry
/// no longer put anything to the user.
const ASK_WINDOW_DAYS: i64 = 14;
/// Ids a Tidy up row lists.
const MAX_TIDY_REFS: usize = 50;
/// The serialized snapshot's ceiling: the host refuses one over 4 MiB
/// outright, and adds its own fields (`recorded_by`, guidance) after.
const SNAPSHOT_BUDGET: usize = 3 * 1024 * 1024 + 512 * 1024;
/// A long text field's length once the snapshot is over budget: the view
/// renders each entry's body from its `span`, so these are previews.
const SHORT_TEXT_BYTES: usize = 280;
const MAX_WARNINGS: usize = 50;

const LIVING: &str = ".living";
const PROTOCOL_FILE: &str = "MYCELIUM.md";
const STATUS_BEGIN: &str = "<!-- BEGIN MYCELIUM LIFECYCLE STATUS -->";
const STATUS_END: &str = "<!-- END MYCELIUM LIFECYCLE STATUS -->";
/// The Stop hook's deterministic fallback handoff (`mycelium-stop-check.sh`)
/// opens its "What was worked on" with this line.
const STUB_LINE: &str = "completed the session work recorded in the finalized session log";

// ---------------------------------------------------------------------------
// Wire shapes
// ---------------------------------------------------------------------------

/// Everything the Knowledge view shows from mycelium. A missing source is an
/// empty section, never an error. Fields added in 0.2.0 sit after the 0.1.3
/// ones and are omitted when empty, so a 0.1.3-shaped input serializes what
/// 0.1.3 did plus only what it newly has (and the constant `id_shapes` and
/// `labels`).
#[derive(Serialize, Clone, Debug, Default)]
pub(crate) struct Knowledge {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) left_off: Option<LeftOff>,
    /// By slug.
    pub(crate) topics: Vec<Topic>,
    /// Newest first; undated last.
    pub(crate) decisions: Vec<Decision>,
    /// Newest first; undated last.
    pub(crate) learnings: Vec<Learning>,
    /// Registry table rows in file order, then the `##` to-do sections.
    pub(crate) todos: Vec<Todo>,
    /// Every finding's open questions, deduplicated.
    pub(crate) questions: Vec<OpenQuestion>,
    pub(crate) counts: Counts,
    /// Human-readable notes: skipped files, legacy formats, caps hit.
    pub(crate) warnings: Vec<String>,
    /// `.living/conventions.md` sections in file order, then the generated
    /// conventions by directory name.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) conventions: Vec<Convention>,
    /// `.living/log/LOG_REGISTRY.md` rows, newest first.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) sessions: Vec<Session>,
    /// "Waiting on you": sentences that put something to the user, newest
    /// first.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) asks: Vec<AskItem>,
    /// "Tidy up": factual inconsistencies in the knowledge itself — never a
    /// status judgment.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) tidy: Vec<Tidy>,
    /// The id shapes this plugin answers for — what a chat may turn into
    /// chips (only ids the snapshot has).
    pub(crate) id_shapes: Vec<IdShape>,
    /// The plugin's words: section names, kinds, the status vocabulary.
    pub(crate) labels: Labels,
    /// The plugin's own guidance files (`MYCELIUM.md` when the workspace
    /// has one); the host appends AGENTS.md, CLAUDE.md and agent memory.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) guidance: Vec<Guidance>,
}

#[derive(Serialize, Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Guidance {
    /// Workspace-relative.
    pub(crate) path: &'static str,
    pub(crate) label: &'static str,
    pub(crate) description: &'static str,
}

/// Where an entry's markdown lives: `path` workspace-relative, `line` its
/// heading (1-based), `end_line` its last line (inclusive; trailing blank
/// lines excluded). The reader fetches the file and renders the slice —
/// bodies never ride the snapshot.
#[derive(Serialize, Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Span {
    pub(crate) path: String,
    pub(crate) line: u32,
    pub(crate) end_line: u32,
}

/// The session handoff ("Where we left off").
#[derive(Serialize, Clone, Debug, Default)]
pub(crate) struct LeftOff {
    pub(crate) worked_on: String,
    pub(crate) decisions: String,
    pub(crate) blockers: Vec<String>,
    pub(crate) current: String,
    pub(crate) next: Vec<String>,
    /// The handoff file's mtime, ms since the epoch.
    pub(crate) written_ms: u64,
    /// Workspace-relative.
    pub(crate) path: String,
    /// Set when the handoff came from a `.mycelium/run/<host>/<session-id>/`
    /// directory — the agent's own id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) host: Option<String>,
    /// The chosen handoff, whole file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) span: Option<Span>,
    /// Every handoff found, newest first (the chosen one included).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) sources: Vec<HandoffSource>,
}

#[derive(Serialize, Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct HandoffSource {
    pub(crate) path: String,
    pub(crate) written_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) host: Option<String>,
}

#[derive(Serialize, Clone, Debug, Default)]
pub(crate) struct Topic {
    /// The file stem.
    pub(crate) slug: String,
    pub(crate) description: String,
    /// Workspace-relative.
    pub(crate) path: String,
    /// By numeric id.
    pub(crate) findings: Vec<Finding>,
    /// Frontmatter `last_updated`.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub(crate) date: String,
}

#[derive(Serialize, Clone, Debug, Default)]
pub(crate) struct Finding {
    /// As written, e.g. "F-003".
    pub(crate) id: String,
    /// The heading after the id (its trailing `(date)` moved to `date`).
    pub(crate) claim: String,
    /// preliminary | supported | robust | contradicted — the Mycelium word
    /// the agent's Status starts with — else unknown. Read, never computed.
    pub(crate) status: String,
    pub(crate) implications: String,
    pub(crate) tags: Vec<String>,
    pub(crate) ledger: Vec<LedgerRow>,
    pub(crate) questions: Vec<String>,
    /// Follow-ups written under `F-NNN addendum…` / `CORRECTION` /
    /// `RESOLVED` / … headings, in file order. Their evidence rows, open
    /// questions and tags are the finding's own.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) addenda: Vec<Addendum>,
    /// 1-based line of the `F-` heading, for "open in file".
    pub(crate) line: u32,
    /// Newest ledger date, else the topic's `last_updated`.
    pub(crate) updated: String,
    /// `<slug>/<id>`, `~2`, `~3` for a repeat in one file: unique even when
    /// ids collide.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub(crate) key: String,
    /// The Status as written, markdown stripped.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub(crate) stated: String,
    /// A trailing `(YYYY-MM-DD)` in the heading, else `**Date**:`.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub(crate) date: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) span: Option<Span>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) refs: Vec<Ref>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) cites: Vec<Cite>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) state: Option<State>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) amends: Vec<Amend>,
    /// The `**Claim:**` statement, when the heading is a title rather than
    /// the claim itself.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub(crate) statement: String,
    #[serde(skip)]
    pub(crate) pending: Pending,
}

/// What an entry holds for the final pass: its ask candidates, whether its
/// state is its own marker (an inverse never overrides one), and whether
/// its newest follow-up is a resolution.
#[derive(Clone, Debug, Default)]
pub(crate) struct Pending {
    /// Each with the day of the part that wrote it (the entry's own date, or
    /// its follow-up's): an old ask must not look new because a later
    /// follow-up was added.
    asks: Vec<(String, Span, Option<i64>)>,
    /// The newest date the entry or its follow-ups carry.
    day: Option<i64>,
    own_state: bool,
    resolved: bool,
    /// A `##` entry mycelium's index doesn't see.
    off_index: bool,
}

#[derive(Serialize, Clone, Debug, Default)]
pub(crate) struct Addendum {
    /// The heading's word and qualifier, e.g. "Addendum (2)", "CORRECTION
    /// (2026-07-23)", "Update".
    pub(crate) label: String,
    /// The heading after its separator; may be empty.
    pub(crate) title: String,
    /// Markdown, as written, less what the finding took: Status and Tags
    /// lines, Evidence and Open questions subsections.
    pub(crate) text: String,
    /// 1-based line of its heading.
    pub(crate) line: u32,
    /// addendum | correction | resolution | update
    #[serde(skip_serializing_if = "str::is_empty")]
    pub(crate) kind: &'static str,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub(crate) date: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub(crate) stated: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) span: Option<Span>,
}

#[derive(Serialize, Clone, Debug, Default)]
pub(crate) struct LedgerRow {
    pub(crate) date: String,
    pub(crate) run: String,
    pub(crate) dataset: String,
    pub(crate) project: String,
    pub(crate) result: String,
    /// supports | contradicts | refines | unknown
    pub(crate) direction: String,
}

#[derive(Serialize, Clone, Debug, Default)]
pub(crate) struct Decision {
    /// [`fingerprint`] of the heading's own date and title (as 0.1.3 read
    /// them, so it never moves) — mycelium's `D-N` ids are positional.
    pub(crate) fp: String,
    pub(crate) date: String,
    pub(crate) title: String,
    pub(crate) context: String,
    pub(crate) decision: String,
    pub(crate) alternatives: Vec<String>,
    pub(crate) rationale: String,
    pub(crate) consequences: String,
    pub(crate) tags: Vec<String>,
    pub(crate) line: u32,
    /// Explicit in the heading (`D-157`, `D1` → `D-1`), else mycelium's
    /// positional `D-<n>` when no entry in the file has an explicit id.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub(crate) id: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub(crate) stated: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) span: Option<Span>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) refs: Vec<Ref>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) cites: Vec<Cite>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) state: Option<State>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) amends: Vec<Amend>,
    #[serde(skip)]
    pub(crate) pending: Pending,
}

#[derive(Serialize, Clone, Debug, Default)]
pub(crate) struct Learning {
    /// [`fingerprint`] — as for decisions.
    pub(crate) fp: String,
    pub(crate) date: String,
    pub(crate) title: String,
    /// gotcha | edge-case | insight | failure | tip | other
    pub(crate) category: String,
    pub(crate) what: String,
    pub(crate) why: String,
    pub(crate) resolution: String,
    pub(crate) tags: Vec<String>,
    pub(crate) line: u32,
    /// As for decisions, with `L-`.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub(crate) id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) span: Option<Span>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) refs: Vec<Ref>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) cites: Vec<Cite>,
    #[serde(skip)]
    pub(crate) pending: Pending,
}

#[derive(Serialize, Clone, Debug, Default)]
pub(crate) struct Todo {
    pub(crate) item: String,
    pub(crate) priority: String,
    pub(crate) status: String,
    pub(crate) category: String,
    pub(crate) date: String,
    pub(crate) author: String,
    /// The item's writeup, workspace-relative (the registry links it relative
    /// to `todo/`); verbatim when absolute, a URL, or escaping the workspace.
    /// Only a real link: a free-text File cell's ids are `refs`, its paths
    /// `cites`.
    pub(crate) file: String,
    /// `todo/<id>`, `todo/r<row>` for an id-less table row, `todo/s<n>` for
    /// an id-less section.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub(crate) key: String,
    /// `#50`, `T-GroupTiers`, or empty.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub(crate) id: String,
    /// The item's lead: its bold opening, else its first sentence; markdown
    /// stripped, ≤ 160 chars.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub(crate) title: String,
    /// The status starts with a closed word (`done 2026-09-23 …`), or a
    /// section's heading says ✅ / DONE / COMPLETE.
    pub(crate) closed: bool,
    /// table | section
    #[serde(skip_serializing_if = "str::is_empty")]
    pub(crate) source: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) span: Option<Span>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) refs: Vec<Ref>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) cites: Vec<Cite>,
}

#[derive(Serialize, Clone, Debug, Default)]
pub(crate) struct OpenQuestion {
    pub(crate) text: String,
    /// The F-id that raised it.
    pub(crate) finding: String,
    /// The raising finding's `key`.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub(crate) key: String,
}

#[derive(Serialize, Clone, Debug, Default)]
pub(crate) struct Counts {
    pub(crate) findings: u32,
    pub(crate) decisions: u32,
    pub(crate) learnings: u32,
    /// Open to-dos plus open questions.
    pub(crate) open: u32,
    /// Open to-dos.
    pub(crate) todos: u32,
    pub(crate) questions: u32,
    pub(crate) conventions: u32,
    pub(crate) sessions: u32,
}

#[derive(Serialize, Clone, Debug, Default)]
pub(crate) struct Convention {
    /// `conventions/<id>`, `conventions/s<n>` for an id-less section,
    /// `generated-conventions/<dir>`.
    pub(crate) key: String,
    /// A leading `C-N`, or a generated convention's frontmatter `id`.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub(crate) id: String,
    pub(crate) title: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub(crate) status: String,
    /// A trailing `(YYYY-MM-DD)` in the heading, else frontmatter `created`.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub(crate) date: String,
    pub(crate) span: Span,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) refs: Vec<Ref>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) cites: Vec<Cite>,
}

#[derive(Serialize, Clone, Debug, Default)]
pub(crate) struct Session {
    pub(crate) id: String,
    pub(crate) date: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub(crate) branch: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub(crate) duration: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub(crate) files: String,
    pub(crate) summary: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub(crate) outputs: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub(crate) status: String,
    /// The Log cell's link, workspace-relative.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub(crate) log: String,
}

#[derive(Serialize, Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct AskItem {
    /// The sentence, markdown stripped, ≤ 300 chars.
    pub(crate) text: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub(crate) date: String,
    pub(crate) source: AskSource,
    pub(crate) span: Span,
}

/// Where an ask was written: a finding (`key` its key), a decision (`key`
/// its fingerprint) or the handoff (`key` its path, `id` its session).
#[derive(Serialize, Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct AskSource {
    pub(crate) kind: &'static str,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub(crate) id: String,
    pub(crate) key: String,
}

#[derive(Serialize, Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Tidy {
    /// duplicate-id | off-index | handoff-stub | duplicate-todo
    pub(crate) kind: &'static str,
    pub(crate) text: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) refs: Vec<Ref>,
    /// The full request an agent would need, naming files and ids.
    pub(crate) ask: String,
}

#[derive(Serialize, Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct IdShape {
    pub(crate) kind: &'static str,
    /// A JavaScript-compatible regex source, without anchors.
    pub(crate) pattern: &'static str,
}

#[derive(Serialize, Clone, Debug, Default)]
pub(crate) struct Labels {
    /// The source chip.
    pub(crate) source: &'static str,
    pub(crate) sections: SectionLabels,
    pub(crate) kinds: KindLabels,
    pub(crate) status_words: Vec<StatusWord>,
    /// Who sets the status — the legend.
    pub(crate) status_note: &'static str,
}

#[derive(Serialize, Clone, Debug, Default)]
pub(crate) struct SectionLabels {
    pub(crate) overview: &'static str,
    pub(crate) left_off: &'static str,
    pub(crate) asks: &'static str,
    pub(crate) changed: &'static str,
    pub(crate) open_work: &'static str,
    pub(crate) findings: &'static str,
    pub(crate) decisions: &'static str,
    pub(crate) learnings: &'static str,
    pub(crate) conventions: &'static str,
    pub(crate) todos: &'static str,
    pub(crate) sessions: &'static str,
    pub(crate) tidy: &'static str,
}

/// Singular display words per kind.
#[derive(Serialize, Clone, Debug, Default)]
pub(crate) struct KindLabels {
    pub(crate) finding: &'static str,
    pub(crate) decision: &'static str,
    pub(crate) learning: &'static str,
    pub(crate) convention: &'static str,
    pub(crate) todo: &'static str,
    pub(crate) session: &'static str,
}

#[derive(Serialize, Clone, Debug, Default)]
pub(crate) struct StatusWord {
    pub(crate) word: &'static str,
    /// 1–3 on mycelium's ladder; 0 for none.
    pub(crate) rank: u8,
    /// neutral | accent | good | warn | bad (ui/1's tones)
    pub(crate) tone: &'static str,
}

/// What [`read`] would read, by metadata: equal stamps mean a cached
/// [`Knowledge`] is still current. It crosses to the host as the snapshot's
/// stamp, `{"files": [[path, mtime_ms, len], …], "refused": [path, …]}`:
/// the host hands it back to ask "changed?", and attributes a new entry to
/// a turn only when its file's mtime here is after that turn started.
#[derive(Serialize, Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Stamp {
    /// `(workspace-relative path, mtime_ms, len)` per file.
    files: Vec<(String, u64, u64)>,
    /// Paths refused as symlinks: one appearing or vanishing changes the
    /// warnings even when no readable file did.
    refused: Vec<String>,
}

// ---------------------------------------------------------------------------
// Entry points
// ---------------------------------------------------------------------------

/// Parse everything `plan` found. Outside a mycelium workspace (no `.living/`
/// and no `MYCELIUM.md`) the plan is empty — a stray `todo/` is not
/// knowledge.
pub(crate) fn read(fs: &impl Fs, plan: Plan) -> Knowledge {
    let Plan {
        sources,
        handoffs,
        protocol,
        mut notes,
        ..
    } = plan;
    let mut knowledge = Knowledge::default();
    if protocol.is_some() {
        knowledge.guidance.push(Guidance {
            path: PROTOCOL_FILE,
            label: PROTOCOL_FILE,
            description: "How agents record knowledge here",
        });
    }
    let mut budget = Budget::default();
    let handoff = read_handoff(fs, &handoffs, &mut budget, &mut notes);
    let mut topics = Vec::new();
    // Topic sources arrive in slug order, so spending this budget as they
    // parse keeps exactly the first findings by (slug, id).
    let mut finding_budget = MAX_ENTRIES;
    let mut findings_seen = 0usize;
    let mut topics_unread = 0usize;
    let mut todo_sections = Vec::new();
    for source in &sources {
        if matches!(source.kind, SourceKind::Topic { .. }) && finding_budget == 0 {
            topics_unread += 1;
            continue;
        }
        let Some(text) = budget.read(fs, source, &mut notes) else {
            continue;
        };
        let rel = source.rel.as_str();
        match &source.kind {
            // Planned separately (`Plan::handoffs`), read above.
            SourceKind::Handoff { .. } => {}
            SourceKind::Topic { slug } => {
                let (topic, found) = parse_topic(&text, rel, slug, finding_budget, &mut notes);
                findings_seen += found;
                if let Some(topic) = topic {
                    finding_budget -= topic.findings.len();
                    topics.push(topic);
                }
            }
            SourceKind::Decisions => knowledge.decisions = parse_decisions(&text, rel, &mut notes),
            SourceKind::Learnings => knowledge.learnings = parse_learnings(&text, rel, &mut notes),
            SourceKind::TodoRegistry | SourceKind::TodoLegacy => {
                let legacy = matches!(source.kind, SourceKind::TodoLegacy);
                let (todos, sections) = parse_todos(&text, rel, legacy, &mut notes);
                knowledge.todos = todos;
                todo_sections = sections;
            }
            SourceKind::Conventions => {
                knowledge.conventions = parse_conventions(&text, rel, &mut notes);
            }
            SourceKind::GeneratedConvention { dir } => {
                knowledge
                    .conventions
                    .extend(parse_generated_convention(&text, rel, dir));
            }
            SourceKind::LogRegistry => {
                knowledge.sessions = parse_sessions(&text, rel, &mut notes);
            }
        }
    }
    budget.note(&mut notes);
    let shown: usize = topics.iter().map(|t| t.findings.len()).sum();
    if findings_seen > shown || topics_unread > 0 {
        let unread = if topics_unread > 0 {
            format!(" ({topics_unread} more topic files not read)")
        } else {
            String::new()
        };
        notes.push(format!(
            "findings: showing the first {shown} of {findings_seen}{unread}"
        ));
    }
    if knowledge.conventions.len() > MAX_CONVENTIONS {
        notes.push(format!(
            "conventions: showing the first {MAX_CONVENTIONS} of {}",
            knowledge.conventions.len()
        ));
        knowledge.conventions.truncate(MAX_CONVENTIONS);
    }
    topics.sort_by(|a, b| a.slug.cmp(&b.slug));

    let mut seen = HashSet::new();
    let mut dropped = 0usize;
    for finding in topics.iter().flat_map(|t| &t.findings) {
        for question in &finding.questions {
            if !seen.insert(collapse_ws(&question.to_lowercase())) {
                continue;
            }
            if knowledge.questions.len() == MAX_ENTRIES {
                dropped += 1;
                continue;
            }
            knowledge.questions.push(OpenQuestion {
                text: question.clone(),
                finding: finding.id.clone(),
                key: finding.key.clone(),
            });
        }
    }
    if dropped > 0 {
        notes.push(format!(
            "open questions: showing the first {MAX_ENTRIES} of {}",
            MAX_ENTRIES + dropped
        ));
    }
    knowledge.topics = topics;
    finish(&mut knowledge, handoff, &todo_sections);
    fit(&mut knowledge, SNAPSHOT_BUDGET, &mut notes);
    knowledge.warnings = notes.finish();
    knowledge
}

/// Keeps the serialized snapshot under `budget`, cheapest loss first: every
/// long text field shortened (the files hold them in full, and the view
/// reads bodies from `span`), then every entry's `cites`, then — a tenth at
/// a time, from whichever section is largest — the oldest entries. Each
/// step says so in `warnings`. It always ends under budget: the last resort
/// empties the lists.
fn fit(k: &mut Knowledge, budget: usize, notes: &mut Notes) {
    let size =
        |k: &Knowledge| chimaera_plugin_api::serde_json::to_vec(k).map_or(usize::MAX, |v| v.len());
    let over = size(k);
    if over <= budget {
        return;
    }
    shorten(k);
    let mib = |n: usize| format!("{:.1} MiB", n as f64 / (1024.0 * 1024.0));
    notes.push(format!(
        "the snapshot was {}, over its {} budget: long text fields are shortened to {SHORT_TEXT_BYTES} bytes (the files hold them in full)",
        mib(over),
        mib(budget)
    ));
    if size(k) <= budget {
        return;
    }
    for f in k.topics.iter_mut().flat_map(|t| t.findings.iter_mut()) {
        f.cites.clear();
    }
    k.decisions.iter_mut().for_each(|d| d.cites.clear());
    k.learnings.iter_mut().for_each(|l| l.cites.clear());
    k.todos.iter_mut().for_each(|t| t.cites.clear());
    k.conventions.iter_mut().for_each(|c| c.cites.clear());
    notes.push("…and the files, scripts and jobs entries cite are left out".to_owned());

    // Then the oldest entries of the largest section, a tenth at a time.
    // Only the section cut is measured again: the snapshot's size moves by
    // exactly its change.
    let counts = |k: &Knowledge| {
        [
            k.topics.iter().map(|t| t.findings.len()).sum::<usize>(),
            k.decisions.len(),
            k.learnings.len(),
            k.todos.len(),
            k.sessions.len(),
            k.questions.len(),
            k.conventions.len(),
        ]
    };
    let before = counts(k);
    let mut total = size(k);
    let mut sizes = [
        section_size(&k.topics),
        section_size(&k.decisions),
        section_size(&k.learnings),
        section_size(&k.todos),
        section_size(&k.sessions),
        section_size(&k.questions),
        section_size(&k.conventions),
    ];
    while total > budget {
        let (largest, &bytes) = sizes
            .iter()
            .enumerate()
            .max_by_key(|&(_, &bytes)| bytes)
            .expect("seven sections");
        if bytes <= 2 {
            break;
        }
        let tenth = |n: usize| (n / 10).max(1);
        let now = match largest {
            // Findings: the lowest ids of the largest topic (the oldest).
            0 => {
                if let Some(topic) = k.topics.iter_mut().max_by_key(|t| t.findings.len()) {
                    let cut = tenth(topic.findings.len()).min(topic.findings.len());
                    topic.findings.drain(..cut);
                }
                k.topics.retain(|t| !t.findings.is_empty());
                section_size(&k.topics)
            }
            // Newest first: the oldest are last.
            1 => {
                k.decisions
                    .truncate(k.decisions.len() - tenth(k.decisions.len()));
                section_size(&k.decisions)
            }
            2 => {
                k.learnings
                    .truncate(k.learnings.len() - tenth(k.learnings.len()));
                section_size(&k.learnings)
            }
            // File order (rows append, sections follow): closed to-dos go
            // first, then the oldest open ones.
            3 => {
                let mut cut = tenth(k.todos.len());
                k.todos.retain(|t| {
                    let drop = cut > 0 && t.closed;
                    cut -= usize::from(drop);
                    !drop
                });
                let rest = cut.min(k.todos.len());
                k.todos.drain(..rest);
                section_size(&k.todos)
            }
            4 => {
                k.sessions
                    .truncate(k.sessions.len() - tenth(k.sessions.len()));
                section_size(&k.sessions)
            }
            5 => {
                k.questions
                    .truncate(k.questions.len() - tenth(k.questions.len()));
                section_size(&k.questions)
            }
            _ => {
                k.conventions
                    .truncate(k.conventions.len() - tenth(k.conventions.len()));
                section_size(&k.conventions)
            }
        };
        total = total - sizes[largest] + now;
        sizes[largest] = now;
    }
    let after = counts(k);
    if after != before {
        notes.push(format!(
            "…and only the newest are shown: {} of {} findings, {} of {} decisions, {} of {} learnings, {} of {} to-dos, {} of {} sessions",
            after[0], before[0], after[1], before[1], after[2], before[2], after[3], before[3], after[4], before[4]
        ));
        let open_todos = k.todos.iter().filter(|t| !t.closed).count();
        k.counts.findings = count(after[0]);
        k.counts.decisions = count(after[1]);
        k.counts.learnings = count(after[2]);
        k.counts.todos = count(open_todos);
        k.counts.open = count(open_todos + k.questions.len());
        k.counts.questions = count(k.questions.len());
        k.counts.conventions = count(k.conventions.len());
        k.counts.sessions = count(k.sessions.len());
    }
}

/// A list's serialized size.
fn section_size<T: Serialize>(list: &[T]) -> usize {
    chimaera_plugin_api::serde_json::to_vec(list).map_or(0, |v| v.len())
}

/// Every long text field cut to [`SHORT_TEXT_BYTES`], and every list a
/// finding repeats (evidence, questions) to its newest few.
fn shorten(k: &mut Knowledge) {
    const KEEP: usize = 5;
    let short = |s: &mut String| {
        if s.len() > SHORT_TEXT_BYTES {
            *s = cap_bytes(std::mem::take(s), SHORT_TEXT_BYTES);
        }
    };
    for f in k.topics.iter_mut().flat_map(|t| t.findings.iter_mut()) {
        for s in [
            &mut f.claim,
            &mut f.implications,
            &mut f.statement,
            &mut f.stated,
        ] {
            short(s);
        }
        f.questions.truncate(KEEP);
        f.questions.iter_mut().for_each(short);
        let rows = f.ledger.len();
        f.ledger.drain(..rows.saturating_sub(KEEP));
        for r in &mut f.ledger {
            for s in [&mut r.run, &mut r.dataset, &mut r.project, &mut r.result] {
                short(s);
            }
        }
        for a in &mut f.addenda {
            for s in [&mut a.label, &mut a.title, &mut a.text, &mut a.stated] {
                short(s);
            }
        }
    }
    k.questions.iter_mut().for_each(|q| short(&mut q.text));
    for d in &mut k.decisions {
        for s in [
            &mut d.title,
            &mut d.context,
            &mut d.decision,
            &mut d.rationale,
            &mut d.consequences,
            &mut d.stated,
        ] {
            short(s);
        }
        d.alternatives.truncate(KEEP);
        d.alternatives.iter_mut().for_each(short);
    }
    for l in &mut k.learnings {
        for s in [&mut l.title, &mut l.what, &mut l.why, &mut l.resolution] {
            short(s);
        }
    }
    for t in &mut k.todos {
        for s in [&mut t.item, &mut t.status, &mut t.category, &mut t.author] {
            short(s);
        }
    }
    for s in &mut k.sessions {
        for f in [&mut s.summary, &mut s.outputs, &mut s.files] {
            short(f);
        }
    }
    for c in &mut k.conventions {
        short(&mut c.title);
    }
    for w in &mut k.warnings {
        short(w);
    }
}

/// Bytes read so far by one [`read`], against [`MAX_TOTAL_BYTES`].
#[derive(Default)]
struct Budget {
    spent: u64,
    over: usize,
}

impl Budget {
    fn read(&mut self, fs: &impl Fs, source: &Source, notes: &mut Notes) -> Option<String> {
        // An oversized file is read_source's to report, not the budget's.
        let len = source.stat.size;
        if len <= MAX_FILE_BYTES && self.spent + len > MAX_TOTAL_BYTES {
            self.over += 1;
            return None;
        }
        let text = read_source(fs, source, notes)?;
        self.spent += text.len() as u64;
        Some(text)
    }

    fn note(&self, notes: &mut Notes) {
        if self.over > 0 {
            notes.push(format!(
                "{} knowledge files not read: the {} per-read budget was spent",
                self.over,
                mib(MAX_TOTAL_BYTES)
            ));
        }
    }
}

/// A stable id for a decision or learning: FNV-1a 64 over the normalized
/// (lowercased, whitespace-collapsed) `kind|date|title`, as 12 hex chars
/// behind the kind's initial — `L-d407379e8c29`. Deliberately not std's
/// `DefaultHasher`, whose output may change between Rust releases: these ids
/// outlive the process (Timeline links, UI state).
pub(crate) fn fingerprint(kind: &str, date: &str, title: &str) -> String {
    let norm = |s: &str| collapse_ws(&s.to_lowercase());
    let key = format!("{}|{}|{}", norm(kind), norm(date), norm(title));
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in key.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    let initial = kind
        .trim()
        .chars()
        .next()
        .map_or('X', |c| c.to_ascii_uppercase());
    format!("{initial}-{:012x}", hash >> 16)
}

// ---------------------------------------------------------------------------
// Discovery: which files a read would open
// ---------------------------------------------------------------------------

#[derive(Debug)]
enum SourceKind {
    Handoff {
        session_id: Option<String>,
        host: Option<String>,
    },
    Topic {
        slug: String,
    },
    Decisions,
    Learnings,
    TodoRegistry,
    TodoLegacy,
    Conventions,
    GeneratedConvention {
        dir: String,
    },
    LogRegistry,
}

#[derive(Debug)]
struct Source {
    kind: SourceKind,
    /// Workspace-relative, `/`-joined.
    rel: String,
    /// The host's stat — the file itself, never a link target.
    stat: Stat,
}

/// The file set behind both [`Plan::stamp`] and [`read`], so the two can
/// never disagree about what a read covers.
#[derive(Default)]
pub(crate) struct Plan {
    sources: Vec<Source>,
    /// Every handoff found: all are stamped (a new one changes `sources`),
    /// only the newest few are read.
    handoffs: Vec<Source>,
    /// `MYCELIUM.md`'s stat: its presence is the `guidance` entry, so it is
    /// stamped though never read.
    protocol: Option<Stat>,
    refused: Vec<String>,
    notes: Notes,
}

impl Plan {
    /// Metadata of every file [`read`] would open — no contents read.
    pub(crate) fn stamp(&self) -> Stamp {
        Stamp {
            files: self
                .handoffs
                .iter()
                .chain(&self.sources)
                .map(|s| (s.rel.clone(), s.stat.mtime_ms, s.stat.size))
                .chain(
                    self.protocol
                        .as_ref()
                        .map(|p| (PROTOCOL_FILE.to_owned(), p.mtime_ms, p.size)),
                )
                .collect(),
            refused: self.refused.clone(),
        }
    }

    /// Whether `rel` is a real directory; a symlink is refused, not followed.
    fn dir(&mut self, fs: &impl Fs, rel: &str) -> bool {
        match fs.stat(rel) {
            Ok(stat) => stat.is_dir,
            Err(err) => {
                if is_symlink_refusal(&err) {
                    self.refuse(rel);
                }
                false
            }
        }
    }

    /// A file at `rel`, not yet queued. The host's stat can't tell a regular
    /// file from a FIFO or socket; its read refuses those.
    fn probe(&mut self, fs: &impl Fs, rel: &str, kind: SourceKind) -> Option<Source> {
        match fs.stat(rel) {
            Ok(stat) => (!stat.is_dir).then(|| Source {
                kind,
                rel: rel.to_owned(),
                stat,
            }),
            Err(err) => {
                if is_symlink_refusal(&err) {
                    self.refuse(rel);
                }
                None
            }
        }
    }

    fn file(&mut self, fs: &impl Fs, rel: &str, kind: SourceKind) -> bool {
        let found = self.probe(fs, rel, kind);
        let queued = found.is_some();
        self.sources.extend(found);
        queued
    }

    fn refuse(&mut self, rel: &str) {
        self.refused.push(rel.to_owned());
        self.notes.push(format!(
            "{rel} is a symlink; not followed (Knowledge reads only real files inside the workspace)"
        ));
    }

    /// Sorted names in `rel`, at most [`MAX_DIR_ENTRIES`] examined (the
    /// host's own listing cap). The host stops at the cap without saying
    /// whether more remain, so a full listing counts as over it.
    fn list(&mut self, fs: &impl Fs, rel: &str) -> Vec<String> {
        let Ok(entries) = fs.list(rel, MAX_DIR_ENTRIES as u32) else {
            return Vec::new();
        };
        if entries.len() >= MAX_DIR_ENTRIES {
            self.notes.push(format!(
                "{rel}: over {MAX_DIR_ENTRIES} entries; only the first {MAX_DIR_ENTRIES} were considered"
            ));
        }
        let mut names: Vec<String> = entries.into_iter().map(|e| e.name).collect();
        names.sort();
        names
    }
}

/// Which files a read covers, by metadata only.
pub(crate) fn plan(fs: &impl Fs) -> Plan {
    let mut plan = Plan::default();
    let living = plan.dir(fs, LIVING);
    let protocol = fs.stat(PROTOCOL_FILE).ok().filter(|stat| !stat.is_dir);
    if !living && protocol.is_none() {
        return plan;
    }
    plan.protocol = protocol;
    // Topic files go last: they are the many-file source, so they are what
    // a spent MAX_TOTAL_BYTES budget should drop.
    plan_handoffs(fs, &mut plan);
    if living {
        plan.file(fs, ".living/decisions.md", SourceKind::Decisions);
        plan.file(fs, ".living/learnings.md", SourceKind::Learnings);
        plan.file(fs, ".living/conventions.md", SourceKind::Conventions);
        plan.file(fs, ".living/log/LOG_REGISTRY.md", SourceKind::LogRegistry);
    }
    if plan.dir(fs, "todo") && !plan.file(fs, "todo/TODO_REGISTRY.md", SourceKind::TodoRegistry) {
        plan.file(fs, "todo/TODOLIST.md", SourceKind::TodoLegacy);
    }
    if living {
        plan_generated_conventions(fs, &mut plan);
        plan_topics(fs, &mut plan);
    }
    plan
}

/// Every handoff: the shared `.mycelium/last-session.md` and each
/// `.mycelium/run/<host>/<session-id>/last-session.md`. The read picks the
/// newest by mtime — the Stop hook's fallback stub in the shared file must
/// not hide a hand-written run handoff written after it.
fn plan_handoffs(fs: &impl Fs, plan: &mut Plan) {
    if !plan.dir(fs, ".mycelium") {
        return;
    }
    let shared = SourceKind::Handoff {
        session_id: None,
        host: None,
    };
    let mut found: Vec<Source> = plan
        .probe(fs, ".mycelium/last-session.md", shared)
        .into_iter()
        .collect();
    if plan.dir(fs, ".mycelium/run") {
        let mut examined = 0usize;
        'hosts: for host in plan.list(fs, ".mycelium/run") {
            let host_rel = format!(".mycelium/run/{host}");
            if !is_run_component(&host) || !plan.dir(fs, &host_rel) {
                continue;
            }
            for session in plan.list(fs, &host_rel) {
                examined += 1;
                if examined > MAX_RUN_DIRS {
                    plan.notes.push(format!(
                        ".mycelium/run: over {MAX_RUN_DIRS} session directories; the rest were not searched for a handoff"
                    ));
                    break 'hosts;
                }
                let dir_rel = format!("{host_rel}/{session}");
                if !is_run_component(&session) || !plan.dir(fs, &dir_rel) {
                    continue;
                }
                let kind = SourceKind::Handoff {
                    session_id: Some(session.clone()),
                    host: Some(host.clone()),
                };
                found.extend(plan.probe(fs, &format!("{dir_rel}/last-session.md"), kind));
            }
        }
    }
    plan.handoffs = found;
}

/// `.living/generated-conventions/<name>/convention.md`, by name.
fn plan_generated_conventions(fs: &impl Fs, plan: &mut Plan) {
    const DIR: &str = ".living/generated-conventions";
    if !plan.dir(fs, DIR) {
        return;
    }
    let names: Vec<String> = plan
        .list(fs, DIR)
        .into_iter()
        .filter(|n| is_run_component(n) && !n.starts_with('.'))
        .collect();
    if names.len() > MAX_GENERATED_CONVENTIONS {
        plan.notes.push(format!(
            "{DIR}: {} entries; read the first {MAX_GENERATED_CONVENTIONS} by name",
            names.len()
        ));
    }
    for name in names.into_iter().take(MAX_GENERATED_CONVENTIONS) {
        let rel = format!("{DIR}/{name}/convention.md");
        plan.file(fs, &rel, SourceKind::GeneratedConvention { dir: name });
    }
}

fn plan_topics(fs: &impl Fs, plan: &mut Plan) {
    const DIR: &str = ".living/findings";
    if !plan.dir(fs, DIR) {
        return;
    }
    let mut names: Vec<String> = plan
        .list(fs, DIR)
        .into_iter()
        .filter(|n| {
            let lower = n.to_ascii_lowercase();
            lower.ends_with(".md")
                && !n.starts_with('.')
                && lower != "index.md"
                && lower != "findings_registry.md"
        })
        .collect();
    // By slug, not file name ("a-b.md" sorts before "a.md"), so the finding
    // budget in `read` is spent in the order the view shows.
    names.sort_by(|a, b| a[..a.len() - 3].cmp(&b[..b.len() - 3]));
    if names.len() > MAX_TOPIC_FILES {
        plan.notes.push(format!(
            "{DIR}: {} topic files; read the first {MAX_TOPIC_FILES} by name",
            names.len()
        ));
        names.truncate(MAX_TOPIC_FILES);
    }
    for name in names {
        let slug = name[..name.len() - 3].to_owned();
        plan.file(fs, &format!("{DIR}/{name}"), SourceKind::Topic { slug });
    }
}

/// mycelium's own session-id rule (`[A-Za-z0-9._-]+`, ≤200, not `.`/`..`);
/// anything else in `run/` isn't a run directory mycelium made.
fn is_run_component(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 200
        && name != "."
        && name != ".."
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// Bounded read of a planned file. Refuses anything that stopped being the
/// regular file the plan saw: the host opens every component `O_NOFOLLOW`
/// and reads only regular files, so a swap to a symlink (or a directory)
/// between the stat and the read is refused there, never followed.
fn read_source(fs: &impl Fs, source: &Source, notes: &mut Notes) -> Option<String> {
    let rel = &source.rel;
    if source.stat.size > MAX_FILE_BYTES {
        notes.push(format!(
            "{rel}: skipped; {} is over the {} read cap",
            mib(source.stat.size),
            mib(MAX_FILE_BYTES)
        ));
        return None;
    }
    // One byte past the cap tells a file that grew past it since the plan.
    let buf = match fs.read(rel, (MAX_FILE_BYTES + 1) as u32) {
        Ok(buf) => buf,
        Err(err) if is_symlink_refusal(&err) || err.ends_with(NOT_REGULAR) => {
            notes.push(format!("{rel}: changed while being read; skipped"));
            return None;
        }
        Err(err) => {
            // The host names the path first; the note already does.
            let why = err.strip_prefix(&format!("{rel}: ")).unwrap_or(&err);
            notes.push(format!("{rel}: unreadable ({why})"));
            return None;
        }
    };
    if buf.len() as u64 > MAX_FILE_BYTES {
        notes.push(format!(
            "{rel}: skipped; grew past the {} read cap",
            mib(MAX_FILE_BYTES)
        ));
        return None;
    }
    let mut text = match String::from_utf8(buf) {
        Ok(text) => text,
        Err(err) => String::from_utf8_lossy(err.as_bytes()).into_owned(),
    };
    if text.starts_with('\u{feff}') {
        text.drain(..'\u{feff}'.len_utf8());
    }
    Some(text)
}

/// Rounded up, so a file a few bytes past the cap never reads as "2.0 MiB".
fn mib(bytes: u64) -> String {
    let tenths = (bytes as f64 / (1024.0 * 1024.0) * 10.0).ceil() / 10.0;
    if tenths.fract() == 0.0 {
        format!("{tenths:.0} MiB")
    } else {
        format!("{tenths:.1} MiB")
    }
}

fn count(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// Warnings, capped so a pathological tree can't grow the list without bound.
#[derive(Default)]
struct Notes {
    list: Vec<String>,
    dropped: usize,
}

impl Notes {
    fn push(&mut self, note: String) {
        if self.list.len() < MAX_WARNINGS {
            self.list.push(cap_text(note));
        } else {
            self.dropped += 1;
        }
    }

    fn finish(mut self) -> Vec<String> {
        if self.dropped > 0 {
            self.list
                .push(format!("…and {} more warnings", self.dropped));
        }
        self.list
    }
}

// ---------------------------------------------------------------------------
// Markdown line model: fences + HTML comments
// ---------------------------------------------------------------------------

/// How a line reads once code fences and HTML comments are accounted for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mark {
    Text,
    /// Inside (or opening/closing) a fenced code block.
    Code,
    /// Inside a multi-line HTML comment, or YAML frontmatter.
    Comment,
}

struct Doc<'a> {
    lines: Vec<&'a str>,
    marks: Vec<Mark>,
    /// 0-based line of a fence left open at end of input.
    unclosed_fence: Option<usize>,
    /// 0-based line of an HTML comment left open at end of input.
    unclosed_comment: Option<usize>,
}

impl<'a> Doc<'a> {
    fn new(text: &'a str) -> Self {
        Self::with_prefix_hidden(text, 0)
    }

    /// Lines before `hidden` (frontmatter) are marked [`Mark::Comment`].
    fn with_prefix_hidden(text: &'a str, hidden: usize) -> Self {
        // Split on '\n' only, like mycelium's `split_log_lines`: line numbers
        // must agree with the files an agent (or "open in file") sees.
        let lines: Vec<&str> = text
            .split('\n')
            .map(|l| l.strip_suffix('\r').unwrap_or(l))
            .collect();
        let mut marks = Vec::with_capacity(lines.len());
        let mut fence: Option<Fence> = None;
        let mut unclosed_fence = None;
        let mut unclosed_comment = None;
        for (i, line) in lines.iter().enumerate() {
            if i < hidden {
                marks.push(Mark::Comment);
                continue;
            }
            if let Some(open) = &fence {
                marks.push(Mark::Code);
                if closes(line, open) {
                    fence = None;
                    unclosed_fence = None;
                }
                continue;
            }
            if unclosed_comment.is_some() {
                marks.push(Mark::Comment);
                if line.contains("-->") {
                    unclosed_comment = None;
                }
                continue;
            }
            if let Some(open) = opening_fence(line) {
                fence = Some(open);
                unclosed_fence = Some(i);
                marks.push(Mark::Code);
                continue;
            }
            // An odd count of backticks before it puts the `<!--` inside a
            // code span: a learning *about* HTML comments must not swallow
            // every entry after it.
            let opens_comment = line.rfind("<!--").is_some_and(|at| {
                !line[at + 4..].contains("-->") && line[..at].matches('`').count() % 2 == 0
            });
            if opens_comment {
                unclosed_comment = Some(i);
                // A line that is only the comment's start carries no content;
                // text before a mid-line opener still does.
                if line.trim_start().starts_with("<!--") {
                    marks.push(Mark::Comment);
                    continue;
                }
            }
            marks.push(Mark::Text);
        }
        Self {
            lines,
            marks,
            unclosed_fence,
            unclosed_comment,
        }
    }

    /// Frontmatter (`---` … `---` opening the file) as `key: value` pairs,
    /// with those lines hidden from the markdown scan: a YAML `# comment`
    /// must not read as a heading.
    fn with_frontmatter(text: &'a str) -> (Self, Vec<(String, String)>) {
        let (pairs, hidden) = frontmatter(text);
        (Self::with_prefix_hidden(text, hidden), pairs)
    }

    fn text(&self, i: usize) -> Option<&'a str> {
        (self.marks.get(i) == Some(&Mark::Text)).then(|| self.lines[i])
    }

    /// Whatever followed an unclosed fence or comment was never examined:
    /// say so rather than present a silently short list.
    fn note_unclosed(&self, rel: &str, notes: &mut Notes) {
        let unclosed = [
            ("code fence", self.unclosed_fence),
            ("HTML comment", self.unclosed_comment),
        ];
        for (what, at) in unclosed {
            if let Some(at) = at {
                notes.push(format!(
                    "{rel}: unclosed {what} at line {}; nothing after it was read as knowledge",
                    at + 1
                ));
            }
        }
    }
}

/// An open fenced code block (mycelium's `markdown_fences.Fence`).
struct Fence {
    marker: u8,
    len: usize,
    /// Visual column of the marker; bounds a matching closer.
    column: usize,
    /// Blockquote depth of the opener; a closer must match it exactly.
    quotes: usize,
}

/// The fence `line` opens: up to three spaces, then any run of list-item or
/// blockquote markers, then ≥3 backticks or tildes — a port of
/// `markdown_fences._FENCE_OPEN_RE`. A backtick fence's info string may not
/// contain a backtick (CommonMark 4.5).
fn opening_fence(line: &str) -> Option<Fence> {
    let b = line.as_bytes();
    let mut i = b.iter().take(3).take_while(|&&c| c == b' ').count();
    let spaces_after = |at: usize| {
        b.get(at..).map_or(0, |rest| {
            rest.iter()
                .take_while(|&&c| matches!(c, b' ' | b'\t'))
                .count()
        })
    };
    loop {
        match b.get(i) {
            Some(b'-' | b'+' | b'*') => {
                let ws = spaces_after(i + 1);
                if ws == 0 {
                    return None;
                }
                i += 1 + ws;
            }
            Some(b'>') => {
                i += 1;
                if matches!(b.get(i), Some(b' ' | b'\t')) {
                    i += 1;
                }
            }
            Some(c) if c.is_ascii_digit() => {
                let digits = b[i..].iter().take_while(|c| c.is_ascii_digit()).count();
                if digits > 9 || !matches!(b.get(i + digits), Some(b'.' | b')')) {
                    return None;
                }
                let ws = spaces_after(i + digits + 1);
                if ws == 0 {
                    return None;
                }
                i += digits + 1 + ws;
            }
            _ => break,
        }
    }
    let marker = *b.get(i)?;
    if marker != b'`' && marker != b'~' {
        return None;
    }
    let len = b[i..].iter().take_while(|&&c| c == marker).count();
    if len < 3 || (marker == b'`' && line[i + len..].contains('`')) {
        return None;
    }
    let prefix = &line[..i];
    Some(Fence {
        marker,
        len,
        column: visual_column(prefix),
        quotes: prefix.matches('>').count(),
    })
}

/// Whether `line` closes `fence` (`markdown_fences.closes`): same character,
/// a run at least as long, nothing after it, the same blockquote depth, and
/// at most three columns past the opener.
fn closes(line: &str, fence: &Fence) -> bool {
    let b = line.as_bytes();
    let i = b
        .iter()
        .take_while(|&&c| matches!(c, b' ' | b'\t' | b'>'))
        .count();
    let len = b[i..].iter().take_while(|&&c| c == fence.marker).count();
    let prefix = &line[..i];
    len >= 3
        && len >= fence.len
        && line[i + len..].trim().is_empty()
        && prefix.matches('>').count() == fence.quotes
        && visual_column(prefix) <= fence.column + 3
}

/// Column where `prefix` ends, tabs expanded to Markdown's 4-column stops.
fn visual_column(prefix: &str) -> usize {
    prefix.chars().fold(0, |col, c| {
        if c == '\t' {
            col + 4 - col % 4
        } else {
            col + 1
        }
    })
}

/// A column-1 ATX heading as `(level, text)`. mycelium's parsers match
/// literal column-1 prefixes, so an indented heading is content here too.
fn heading(line: &str) -> Option<(usize, &str)> {
    let level = line.bytes().take_while(|&b| b == b'#').count();
    if level == 0 || level > 6 {
        return None;
    }
    let rest = &line[level..];
    if !rest.is_empty() && !rest.starts_with([' ', '\t']) {
        return None;
    }
    let text = rest.trim();
    // An optional closing run of '#' (CommonMark), but "C#" keeps its '#'.
    let unclosed = text.trim_end_matches('#');
    if unclosed.len() != text.len() && (unclosed.is_empty() || unclosed.ends_with([' ', '\t'])) {
        return Some((level, unclosed.trim_end()));
    }
    Some((level, text))
}

fn is_thematic_break(line: &str) -> bool {
    if line.len() - line.trim_start().len() > 3 {
        return false;
    }
    let mut chars = line.chars().filter(|c| !c.is_whitespace());
    let Some(first) = chars.next() else {
        return false;
    };
    let mut n = 1;
    for c in chars {
        if c != first {
            return false;
        }
        n += 1;
    }
    matches!(first, '-' | '*' | '_') && n >= 3
}

/// Leading `---` block: `key: value` pairs and the line count it spans (0
/// when there is none). Only flat scalars and folded/literal blocks — the
/// shapes mycelium's topic template writes; no YAML crate for five keys.
fn frontmatter(text: &str) -> (Vec<(String, String)>, usize) {
    let lines: Vec<&str> = text.split('\n').take(200).collect();
    let Some(start) = lines.iter().position(|l| !l.trim().is_empty()) else {
        return (Vec::new(), 0);
    };
    if lines[start].trim() != "---" {
        return (Vec::new(), 0);
    }
    let Some(end) = lines[start + 1..]
        .iter()
        .position(|l| matches!(l.trim(), "---" | "..."))
        .map(|at| start + 1 + at)
    else {
        return (Vec::new(), 0);
    };
    let mut pairs: Vec<(String, String)> = Vec::new();
    let mut block: Option<usize> = None;
    for &line in &lines[start + 1..end] {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if let Some(at) = block {
            if line.starts_with([' ', '\t']) {
                let value = &mut pairs[at].1;
                if !value.is_empty() {
                    value.push(' ');
                }
                value.push_str(line.trim());
                continue;
            }
            block = None;
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim();
        if key.is_empty()
            || line.starts_with([' ', '\t', '#'])
            || !key
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
        {
            continue;
        }
        let value = value.trim();
        if matches!(value, "" | ">" | "|" | ">-" | "|-" | ">+" | "|+") {
            block = Some(pairs.len());
            pairs.push((key.to_ascii_lowercase(), String::new()));
            continue;
        }
        pairs.push((key.to_ascii_lowercase(), unquote(value).to_owned()));
    }
    (pairs, end + 1)
}

fn unquote(value: &str) -> &str {
    for quote in ['"', '\''] {
        if let Some(inner) = value
            .strip_prefix(quote)
            .and_then(|v| v.strip_suffix(quote))
        {
            return inner;
        }
    }
    value
}

// ---------------------------------------------------------------------------
// Text helpers
// ---------------------------------------------------------------------------

/// Cut to [`MAX_TEXT_BYTES`] (the "…" included) on a char boundary.
fn cap_text(s: String) -> String {
    cap_bytes(s, MAX_TEXT_BYTES)
}

/// Cut to `max` bytes (the "…" included) on a char boundary.
fn cap_bytes(mut s: String, max: usize) -> String {
    if s.len() <= max {
        return s;
    }
    let mut end = max - '…'.len_utf8();
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s.truncate(end);
    s.push('…');
    s
}

/// Joined lines, trimmed, with each run of blank lines collapsed to one.
fn tidy<S: AsRef<str>>(lines: &[S]) -> String {
    let mut out = String::new();
    let mut gap = false;
    for line in lines {
        let line = line.as_ref().trim_end();
        if line.trim().is_empty() {
            gap = !out.is_empty();
            continue;
        }
        if !out.is_empty() {
            out.push('\n');
            if gap {
                out.push('\n');
            }
        }
        gap = false;
        out.push_str(line);
    }
    out.trim_start().to_owned()
}

fn strip_inline_comments(line: &str) -> Cow<'_, str> {
    if !line.contains("<!--") {
        return Cow::Borrowed(line);
    }
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(open) = rest.find("<!--") {
        out.push_str(&rest[..open]);
        match rest[open + 4..].find("-->") {
            Some(close) => rest = &rest[open + 4 + close + 3..],
            // Continues on later lines, which the Doc marks as Comment.
            None => rest = "",
        }
    }
    out.push_str(rest);
    Cow::Owned(out)
}

fn unbold(s: &str) -> &str {
    let t = s.trim();
    match t.strip_prefix("**").and_then(|t| t.strip_suffix("**")) {
        Some(inner) if !inner.trim().is_empty() => inner.trim(),
        _ => t,
    }
}

/// A heading's leading date and title — a port of mycelium's
/// `split_entry_date_and_title`. Only a LEADING `[YYYY-MM-DD]` (or bare date)
/// is metadata; a date later in the title belongs to the title.
fn split_date_title(heading_text: &str) -> (&str, &str) {
    let text = unbold(heading_text);
    let (date, rest) = leading_date(text);
    let Some(date) = date else {
        return ("", text);
    };
    let title =
        rest.trim_matches(|c: char| c.is_whitespace() || matches!(c, ':' | '-' | '–' | '—'));
    (date, if title.is_empty() { text } else { title })
}

fn leading_date(text: &str) -> (Option<&str>, &str) {
    let s = text.trim_start();
    let s = s.strip_prefix('[').unwrap_or(s);
    match s.as_bytes().get(..10) {
        Some(head) if is_iso_date(head) => {
            let rest = &s[10..];
            (Some(&s[..10]), rest.strip_prefix(']').unwrap_or(rest))
        }
        _ => (None, text),
    }
}

/// Filler that says nothing: empty/"None"/"TBD" answers, a template's
/// `[prompt]` or `{prompt}`, and the lines mycelium writes as scaffolding —
/// the five-section template's prompts and the Stop hook's deterministic
/// fallback handoff (`mycelium-stop-check.sh`).
fn is_placeholder(s: &str) -> bool {
    const EMPTY: &[&str] = &[
        "none",
        "none yet",
        "none so far",
        "no blockers",
        "nothing",
        "nothing yet",
        "n/a",
        "na",
        "tbd",
        "todo",
        "-",
        "—",
        "–",
        "…",
        "yyyy-mm-dd",
    ];
    const STUBS: &[&str] = &[
        "[decision]:",
        "[resolved/unresolved]:",
        "branch: x | tests: n passing",
        "completed the session work recorded in the finalized session log",
        "see `.living/decisions.md` for decisions recorded during this session",
        "see the finalized session log and `.living/learnings.md` for recorded issues",
        "review the finalized session log and continue from the current branch state",
    ];
    let t = s.trim().trim_matches(|c: char| {
        c.is_whitespace() || matches!(c, '_' | '*' | '`' | '(' | ')' | '.' | '!')
    });
    if t.is_empty()
        || (t.starts_with('[') && t.ends_with(']') && !t.contains("]("))
        || (t.starts_with('{') && t.ends_with('}'))
    {
        return true;
    }
    let lower = t.to_lowercase();
    EMPTY.contains(&lower.as_str()) || STUBS.iter().any(|stub| lower.starts_with(stub))
}

fn first_word(s: &str) -> String {
    s.to_lowercase()
        .split(|c: char| !(c.is_alphanumeric() || c == '-'))
        .find(|w| !w.is_empty())
        .unwrap_or("")
        .to_owned()
}

fn normalize_status(value: &str) -> String {
    let word = first_word(value);
    match word.as_str() {
        "preliminary" | "supported" | "robust" | "contradicted" => word,
        _ => "unknown".to_owned(),
    }
}

fn normalize_category(value: &str) -> String {
    // An unfilled template choice list ("[gotcha|edge-case|…]") is no answer.
    if value.contains('|') {
        return "other".to_owned();
    }
    let category = match first_word(value).as_str() {
        "gotcha" | "gotchas" => "gotcha",
        "edge-case" | "edge-cases" | "edge" | "edgecase" => "edge-case",
        "insight" | "insights" => "insight",
        "failure" | "failures" | "fail" | "failed" => "failure",
        "tip" | "tips" => "tip",
        _ => "other",
    };
    category.to_owned()
}

fn normalize_direction(value: &str) -> String {
    let lower = value.to_lowercase();
    let direction = lower
        .split(|c: char| !c.is_alphanumeric())
        .find_map(|w| {
            if w.starts_with("support") {
                Some("supports")
            } else if w.starts_with("contradict") {
                Some("contradicts")
            } else if w.starts_with("refin") {
                Some("refines")
            } else {
                None
            }
        })
        .unwrap_or("unknown");
    direction.to_owned()
}

/// mycelium's closed todo statuses (`complete`, `wont-do`) and their obvious
/// spellings, as the status's FIRST word: `done 2026-09-23 (applied …)`,
/// `**done**`, `won't do — superseded` are closed; `half done`, `not done`
/// are not.
fn is_closed(status: &str) -> bool {
    const CLOSED: &[&str] = &[
        "complete",
        "completed",
        "done",
        "closed",
        "resolved",
        "wont-do",
        "wontdo",
        "wont-fix",
        "wontfix",
        "cancelled",
        "canceled",
        "dropped",
    ];
    // `✅ done`, `✓ complete`: a check mark leading the status is the word.
    let status = status.trim_start();
    if status.starts_with(['✅', '✓', '✔', '☑']) {
        return true;
    }
    let norm: String = status
        .to_lowercase()
        .chars()
        .filter(|c| !matches!(c, '\'' | '’' | '`' | '*'))
        .map(|c| if c == ' ' || c == '_' { '-' } else { c })
        .collect();
    let norm = norm.trim_matches('-');
    CLOSED.iter().any(|word| {
        norm.strip_prefix(word)
            .is_some_and(|rest| !rest.starts_with(|c: char| c.is_alphanumeric()))
    })
}

/// Tags as `[a, b]`, `a, b`, or `#a #b`; deduplicated case-insensitively.
fn parse_tags(raw: &str, tags: &mut Vec<String>) {
    let raw = raw.trim();
    if raw.starts_with('{') && raw.ends_with('}') {
        return;
    }
    let raw = raw
        .strip_prefix('[')
        .and_then(|r| r.strip_suffix(']'))
        .unwrap_or(raw);
    for piece in raw.split(',') {
        let piece = piece.trim().trim_end_matches('.');
        // `mitigation_type=process` is a key, not a tag.
        if piece.contains('=') {
            continue;
        }
        let words: Vec<&str> = piece.split_whitespace().collect();
        if words.len() > 1 && words.iter().all(|w| w.starts_with('#')) {
            for word in words {
                push_tag(word, tags);
            }
        } else {
            push_tag(piece, tags);
        }
    }
}

fn push_tag(raw: &str, tags: &mut Vec<String>) {
    let tag = raw
        .trim()
        .trim_matches(['[', ']', '`', '"', '\''])
        .trim_start_matches('#')
        .trim();
    if tag.is_empty() || tags.len() >= MAX_TAGS || tags.iter().any(|t| t.eq_ignore_ascii_case(tag))
    {
        return;
    }
    tags.push(cap_text(tag.to_owned()));
}

// ---------------------------------------------------------------------------
// Lists and tables
// ---------------------------------------------------------------------------

struct ListItem {
    text: String,
    /// A checked task box or a fully struck-through item: resolved.
    done: bool,
}

/// A list marker (`-`, `*`, `+`, `1.`, `1)`) and the text after it.
fn list_marker(s: &str) -> Option<&str> {
    let b = s.as_bytes();
    let after = match *b.first()? {
        b'-' | b'*' | b'+' => 1,
        c if c.is_ascii_digit() => {
            let digits = b.iter().take_while(|c| c.is_ascii_digit()).count();
            if digits > 9 || !matches!(b.get(digits), Some(b'.' | b')')) {
                return None;
            }
            digits + 1
        }
        _ => return None,
    };
    match b.get(after) {
        None => Some(""),
        Some(b' ' | b'\t') => Some(s[after..].trim_start()),
        _ => None,
    }
}

/// Top-level list items with their continuation lines (indented or lazy)
/// and nested items folded in.
fn list_items<'s>(lines: impl IntoIterator<Item = &'s str>) -> Vec<ListItem> {
    let mut items: Vec<ListItem> = Vec::new();
    let mut base: Option<usize> = None;
    let mut open = false;
    let mut gap = false;
    for line in lines {
        let body = line.trim_start();
        if body.is_empty() {
            gap = true;
            continue;
        }
        let indent = line.len() - body.len();
        let marker = if is_thematic_break(line) {
            None
        } else {
            list_marker(body)
        };
        if let Some(rest) = marker {
            let base = *base.get_or_insert(indent);
            if indent <= base + 1 {
                items.push(ListItem {
                    text: rest.to_owned(),
                    done: false,
                });
                open = true;
                gap = false;
                continue;
            }
        }
        match items.last_mut() {
            Some(last) if open && (indent > 0 || !gap) => {
                last.text.push(' ');
                last.text.push_str(marker.unwrap_or(body));
                gap = false;
            }
            _ => open = false,
        }
    }
    for item in &mut items {
        let text = collapse_ws(&item.text);
        let (text, done) = match text
            .strip_prefix("[x] ")
            .or_else(|| text.strip_prefix("[X] "))
        {
            Some(rest) => (rest.to_owned(), true),
            None => match text.strip_prefix("[ ] ") {
                Some(rest) => (rest.to_owned(), false),
                None => {
                    let struck = text.len() > 4 && text.starts_with("~~") && text.ends_with("~~");
                    (text, struck)
                }
            },
        };
        item.text = text;
        item.done = done;
    }
    items
}

/// A list slot that says there is nothing, with a reason attached — "None.
/// The claim holds by definition.", "None — resolved", "No open questions:
/// …". What follows the lead word decides: punctuation means "none", a word
/// means a real item ("None of the samples replicate — why?" is a question).
fn says_none(s: &str) -> bool {
    let t = s.trim().trim_start_matches(['*', '_', '`']).to_lowercase();
    [
        "none",
        "no open questions",
        "no questions",
        "nothing open",
        "n/a",
    ]
    .iter()
    .any(|lead| {
        t.strip_prefix(lead).is_some_and(|rest| {
            let rest = rest.trim_start_matches(['*', '_', '`']).trim_start();
            rest.is_empty() || rest.starts_with(['.', ':', ',', ';', '—', '–', '-', '!', '('])
        })
    })
}

/// Kept (not done, not filler) item texts, capped.
fn live_items(items: Vec<ListItem>, cap: usize) -> Vec<String> {
    items
        .into_iter()
        .filter(|item| !item.done && !is_placeholder(&item.text) && !says_none(&item.text))
        .take(cap)
        .map(|item| cap_text(item.text))
        .collect()
}

/// A `|`-delimited table row's cells (`\|` escapes a pipe); the trailing
/// pipe is optional.
fn table_cells(line: &str) -> Option<Vec<String>> {
    let inner = line.trim().strip_prefix('|')?;
    let inner = match inner.strip_suffix('|') {
        Some(stripped) if !stripped.ends_with('\\') => stripped,
        _ => inner,
    };
    let mut cells = Vec::new();
    let mut cell = String::new();
    let mut chars = inner.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if chars.peek() == Some(&'|') => {
                cell.push('|');
                chars.next();
            }
            '|' => cells.push(std::mem::take(&mut cell).trim().to_owned()),
            _ => cell.push(c),
        }
    }
    cells.push(cell.trim().to_owned());
    Some(cells)
}

fn is_separator(cells: &[String]) -> bool {
    !cells.is_empty()
        && cells.iter().any(|c| c.contains('-'))
        && cells
            .iter()
            .all(|c| !c.is_empty() && c.chars().all(|ch| matches!(ch, '-' | ':' | ' ')))
}

/// A table cell as text: links unwrapped, a cell that is one code span
/// (`` `blocked` ``) unwrapped, a lone dash/em-dash ("nothing here")
/// dropped. Code spans inside it stay whole ("Fix `a` and `b`").
fn cell_text(cell: &str) -> String {
    let (text, _) = strip_links(cell);
    let text = text.trim();
    let text = match text.strip_prefix('`').and_then(|t| t.strip_suffix('`')) {
        Some(inner) if !inner.contains('`') => inner.trim(),
        _ => text,
    };
    if matches!(text, "-" | "—" | "–") {
        String::new()
    } else {
        cap_text(text.to_owned())
    }
}

/// A [`Span`] over 0-based lines `start..end` (exclusive), trailing blank
/// lines excluded.
fn span_of(doc: &Doc, rel: &str, start: usize, end: usize) -> Span {
    let mut last = end.min(doc.lines.len()).max(start + 1);
    // Trailing blank lines, a `---` between entries and a trailing comment
    // (`<!-- Add new entries above this line -->`) are not the entry's.
    while last > start + 1 {
        let line = doc.lines[last - 1];
        let trailing = line.trim().is_empty()
            || (doc.marks[last - 1] == Mark::Text && is_thematic_break(line))
            || doc.marks[last - 1] == Mark::Comment
            || (line.trim_start().starts_with("<!--") && line.trim_end().ends_with("-->"));
        if !trailing {
            break;
        }
        last -= 1;
    }
    Span {
        path: rel.to_owned(),
        line: line_no(start),
        end_line: line_no(last - 1),
    }
}

/// The text lines of `ranges` (fences and comments skipped) as paragraphs:
/// runs of non-blank lines, where a heading or a list item starts its own.
/// Each is `(first line, last line, joined text)`, lines 0-based.
fn paragraphs_in(doc: &Doc, ranges: &[Range<usize>]) -> Vec<(usize, usize, String)> {
    let mut out: Vec<(usize, usize, String)> = Vec::new();
    for range in ranges {
        let mut open = false;
        for i in range.start..range.end.min(doc.lines.len()) {
            let Some(line) = doc.text(i) else {
                open = false;
                continue;
            };
            let body = line.trim();
            if body.is_empty() {
                open = false;
                continue;
            }
            let is_heading = heading(line).is_some();
            let starts = is_heading || !open || list_marker(body).is_some();
            let text = heading(line).map_or(body, |(_, text)| text);
            match out.last_mut() {
                Some(last) if !starts => {
                    last.1 = i;
                    last.2.push(' ');
                    last.2.push_str(text);
                }
                _ => out.push((i, i, text.to_owned())),
            }
            open = !is_heading;
        }
    }
    out
}

/// The first paragraph of `range` that is prose — not a heading, not a
/// field line.
fn first_prose(doc: &Doc, range: &Range<usize>, known: &[&str]) -> Option<String> {
    paragraphs_in(doc, std::slice::from_ref(range))
        .into_iter()
        .find(|(first, _, _)| {
            let line = doc.lines[*first];
            heading(line).is_none() && field_line(line, known).is_none()
        })
        .map(|(_, _, text)| text)
}

/// Ask candidates in `ranges`: sentences that put something to the user,
/// each with its paragraph's span, at most [`MAX_ASKS_PER_ENTRY`].
fn asks_in(doc: &Doc, rel: &str, ranges: &[Range<usize>]) -> Vec<(String, Span)> {
    let mut out = Vec::new();
    for (first, last, text) in paragraphs_in(doc, ranges) {
        for sentence in ask_sentences(&text) {
            if out.len() == MAX_ASKS_PER_ENTRY {
                return out;
            }
            out.push((sentence, span_of(doc, rel, first, last + 1)));
        }
    }
    out
}

/// Refs and cites over the text lines of `ranges`, `own` excluded.
fn refs_and_cites(doc: &Doc, ranges: &[Range<usize>], own: &str) -> (Vec<Ref>, Vec<Cite>) {
    let mut refs = Vec::new();
    let mut cites = Vec::new();
    for range in ranges {
        for i in range.start..range.end.min(doc.lines.len()) {
            if let Some(line) = doc.text(i) {
                let line = strip_inline_comments(line);
                scan_refs(&line, own, &mut refs);
                scan_cites(&line, &mut cites);
            }
        }
    }
    (refs, cites)
}

/// Amends over the text lines of `ranges`, plus `F-037 RETRACTED` in the
/// heading and first lines (`lead`).
fn amends_in(doc: &Doc, ranges: &[Range<usize>], lead: &[&str], own: &str) -> Vec<Amend> {
    let mut amends = Vec::new();
    for range in ranges {
        for i in range.start..range.end.min(doc.lines.len()) {
            if let Some(line) = doc.text(i) {
                scan_amends(line, own, &mut amends);
            }
        }
    }
    for line in lead {
        scan_retracted_ids(line, own, &mut amends);
    }
    amends
}

/// The first `n` non-blank text lines of `ranges`.
fn first_lines<'a>(doc: &Doc<'a>, ranges: &[Range<usize>], n: usize) -> Vec<&'a str> {
    ranges
        .iter()
        .flat_map(|r| r.start..r.end.min(doc.lines.len()))
        .filter_map(|i| doc.text(i))
        .filter(|l| !l.trim().is_empty())
        .take(n)
        .collect()
}

// ---------------------------------------------------------------------------
// Bold fields
// ---------------------------------------------------------------------------

const DECISION_FIELDS: &[&str] = &[
    "context",
    "decision",
    "alternatives considered",
    "rationale",
    "consequences",
    "tags",
    "date",
    "status",
    "why",
];
/// Includes the fields mycelium's learning template adds that we don't show
/// (`mitigation_type`, …) — as known labels they end the field before them
/// even in their plain `label: value` form.
const LEARNING_FIELDS: &[&str] = &[
    "category",
    "what happened",
    "why it matters",
    "resolution",
    "tags",
    "mitigation type",
    "structural mitigation candidate",
    "source",
    "symptom",
    "how to apply",
    "generalisable rule",
    "generalizable rule",
    "date",
];
const FINDING_FIELDS: &[&str] = &["status", "claim", "implications", "tags", "date"];
const TODO_FIELDS: &[&str] = &[
    "status", "priority", "category", "author", "owner", "date", "opened", "raised", "added",
    "tags",
];
const CONVENTION_FIELDS: &[&str] = &["status", "tags"];

fn field_label(raw: &str) -> String {
    collapse_ws(&raw.replace('_', " ").to_lowercase())
}

/// A field-opening line: `**Label**: value`, `**Label:** value` or a lead-in
/// `**Label.** prose` with any label; for the kind's known labels only, also
/// `**Label: value**`, `- **Label**: value` and plain `Label: value`
/// (mycelium's own tag reader accepts `Tags: x`). A blockquote prefix is
/// tolerated, as mycelium's is.
fn field_line<'l>(line: &'l str, known: &[&str]) -> Option<(String, Cow<'l, str>)> {
    let s = line.trim_start_matches(|c: char| c == '>' || c.is_whitespace());
    let (s, bulleted) = match s.strip_prefix(['-', '*', '+']) {
        Some(rest) if rest.starts_with([' ', '\t']) => (rest.trim_start(), true),
        _ => (s, false),
    };
    if let Some(inner) = s.strip_prefix("**") {
        let close = inner.find("**")?;
        let raw = &inner[..close];
        let after = &inner[close + 2..];
        let colon = match raw.strip_suffix(':') {
            Some(label) => Some((label, after)),
            None => after.trim_start().strip_prefix(':').map(|v| (raw, v)),
        };
        let (label, value) = if let Some((label, value)) = colon {
            (label, Cow::Borrowed(value.trim()))
        } else if let Some((label, inside)) = raw
            .split_once(':')
            .filter(|(label, _)| known.contains(&field_label(label).as_str()))
        {
            // `**Status: open, on purpose.**`
            let value = format!("{}{}", inside.trim(), after);
            (label, Cow::Owned(value.trim().to_owned()))
        } else if let Some(label) = raw
            .strip_suffix('.')
            .filter(|label| !label.contains([':', '.', '!', '?']))
        {
            // `**Setup.** prose`, `**Why it matters.** …`: a lead-in label.
            (label, Cow::Borrowed(after.trim()))
        } else {
            return None;
        };
        let label = label.trim();
        if label.is_empty() || label.len() > 48 || label.contains('*') {
            return None;
        }
        let label = field_label(label);
        if bulleted && !known.contains(&label.as_str()) {
            return None;
        }
        return Some((label, value));
    }
    if bulleted {
        return None;
    }
    let (label, value) = s.split_once(':')?;
    let label = field_label(label);
    known
        .contains(&label.as_str())
        .then(|| (label, Cow::Borrowed(value.trim())))
}

/// A field line's value split where agents join several fields on one line
/// — `**Date**: 2026-09-28 · **Status**: DECIDED · **Tags**: a, b`: at each
/// `·` that a bold field label follows.
fn split_inline<'l>(
    label: String,
    value: Cow<'l, str>,
    known: &[&str],
) -> Vec<(String, Cow<'l, str>)> {
    if !value.contains('·') {
        return vec![(label, value)];
    }
    match value {
        Cow::Borrowed(v) => split_inline_str(label, v, known),
        Cow::Owned(v) => split_inline_str(label, &v, known)
            .into_iter()
            .map(|(label, value)| (label, Cow::Owned(value.into_owned())))
            .collect(),
    }
}

fn split_inline_str<'s>(label: String, v: &'s str, known: &[&str]) -> Vec<(String, Cow<'s, str>)> {
    // Each `·` is checked against its own segment only (up to the next
    // one), so a long line stays linear; a line of more is prose.
    const MAX_DOTS: usize = 64;
    let dots: Vec<usize> = v
        .match_indices('·')
        .map(|(at, _)| at)
        .take(MAX_DOTS)
        .collect();
    let cuts: Vec<usize> = dots
        .iter()
        .enumerate()
        .filter(|&(k, &at)| {
            let end = dots.get(k + 1).copied().unwrap_or(v.len());
            let next = v[at + '·'.len_utf8()..end].trim_start();
            next.starts_with("**") && field_line(next, known).is_some()
        })
        .map(|(_, &at)| at)
        .collect();
    let mut out = Vec::with_capacity(cuts.len() + 1);
    let first_end = cuts.first().copied().unwrap_or(v.len());
    out.push((label, Cow::Borrowed(v[..first_end].trim())));
    for (k, &at) in cuts.iter().enumerate() {
        let end = cuts.get(k + 1).copied().unwrap_or(v.len());
        let segment = v[at + '·'.len_utf8()..end].trim();
        if let Some(field) = field_line(segment, known) {
            out.push(field);
        }
    }
    out
}

/// An entry's bold fields, each with its value lines: the text after the
/// label plus every following line up to the next field, heading, or
/// thematic break. Fenced lines stay in the value (a snippet under "What
/// happened"); commented lines don't.
struct Fields<'a> {
    list: Vec<(String, Vec<Cow<'a, str>>)>,
}

impl<'a> Fields<'a> {
    /// Over `ranges` of lines, each read as if the others weren't there.
    fn parse(doc: &Doc<'a>, ranges: &[Range<usize>], known: &[&str]) -> Self {
        let mut list: Vec<(String, Vec<Cow<'a, str>>)> = Vec::new();
        let lines = ranges
            .iter()
            .flat_map(|r| (r.start..r.end.min(doc.lines.len())).map(move |i| (i, i == r.start)));
        let mut current = false;
        for (i, first) in lines {
            if first {
                current = false;
            }
            let line = doc.lines[i];
            match doc.marks[i] {
                Mark::Comment => continue,
                Mark::Code => {
                    if let (true, Some(field)) = (current, list.last_mut()) {
                        field.1.push(Cow::Borrowed(line));
                    }
                    continue;
                }
                Mark::Text => {}
            }
            if heading(line).is_some() || is_thematic_break(line) {
                current = false;
                continue;
            }
            if let Some((label, value)) = field_line(line, known) {
                for (label, value) in split_inline(label, value, known) {
                    let value = match value {
                        Cow::Borrowed(v) => strip_inline_comments(v),
                        Cow::Owned(v) => Cow::Owned(strip_inline_comments(&v).into_owned()),
                    };
                    list.push((label, vec![value]));
                }
                current = true;
                continue;
            }
            if let (true, Some(field)) = (current, list.last_mut()) {
                let stripped = strip_inline_comments(line);
                if stripped.trim().is_empty() && !line.trim().is_empty() {
                    continue;
                }
                field.1.push(stripped);
            }
        }
        Self { list }
    }

    /// The first field, in file order, named any of `names` (a label and its
    /// aliases). A repeated field keeps its first value, as mycelium's tag
    /// reader does.
    fn get(&self, names: &[&str]) -> Option<&[Cow<'a, str>]> {
        self.list
            .iter()
            .find(|(label, _)| names.contains(&label.as_str()))
            .map(|(_, lines)| lines.as_slice())
    }

    fn text(&self, names: &[&str]) -> String {
        self.get(names)
            .map_or_else(String::new, |lines| cap_text(tidy(lines)))
    }

    /// The value on the label's own line.
    fn first(&self, names: &[&str]) -> Option<&str> {
        self.get(names)
            .and_then(|lines| lines.first())
            .map(|v| v.as_ref())
    }

    /// A field's first paragraph as written, markdown stripped — `stated`.
    fn stated(&self, names: &[&str]) -> String {
        let Some(lines) = self.get(names) else {
            return String::new();
        };
        let para: Vec<&str> = lines
            .iter()
            .map(|l| l.as_ref())
            .take_while(|l| !l.trim().is_empty())
            .collect();
        cap_text(plain(&para.join(" ")))
    }

    /// A date field's `YYYY-MM-DD`.
    fn date(&self, names: &[&str]) -> Option<String> {
        let value = self.first(names)?;
        let value = plain(value);
        let head = value.get(..10)?;
        is_iso_date(head.as_bytes()).then(|| head.to_owned())
    }

    /// A list-valued field (Alternatives considered): an inline value, the
    /// `- ` items below it, or failing both its paragraphs.
    fn items(&self, names: &[&str]) -> Vec<String> {
        let Some((first, rest)) = self.get(names).and_then(|lines| lines.split_first()) else {
            return Vec::new();
        };
        let mut items = Vec::new();
        let inline = first.trim();
        if !inline.is_empty() {
            items.push(ListItem {
                text: inline.to_owned(),
                done: false,
            });
        }
        let listed = list_items(rest.iter().map(|l| l.as_ref()));
        if listed.is_empty() && inline.is_empty() {
            items.extend(
                paragraphs(rest)
                    .into_iter()
                    .map(|text| ListItem { text, done: false }),
            );
        }
        items.extend(listed);
        live_items(items, MAX_LIST_ITEMS)
    }

    fn tags(&self) -> Vec<String> {
        let mut tags = Vec::new();
        let Some((first, rest)) = self.get(&["tags"]).and_then(|lines| lines.split_first()) else {
            return tags;
        };
        if first.trim().is_empty() {
            // `**Tags**:` over a bullet list.
            for item in list_items(rest.iter().map(|l| l.as_ref())) {
                parse_tags(&item.text, &mut tags);
            }
        } else {
            // The label's own line, and the lines it wraps onto (a line
            // ending in a comma continues); whatever else follows (a
            // `source:` note, a stray paragraph) is not tags.
            parse_tags(first, &mut tags);
            let mut prev = first.trim_end();
            for line in rest {
                if !prev.ends_with(',') || line.trim().is_empty() {
                    break;
                }
                parse_tags(line, &mut tags);
                prev = line.trim_end();
            }
        }
        tags
    }
}

fn paragraphs<S: AsRef<str>>(lines: &[S]) -> Vec<String> {
    tidy(lines)
        .split("\n\n")
        .map(collapse_ws)
        .filter(|p| !p.is_empty())
        .collect()
}

// ---------------------------------------------------------------------------
// Decisions and learnings
// ---------------------------------------------------------------------------

struct EntrySpan<'a> {
    /// 0-based heading line.
    start: usize,
    /// Exclusive.
    end: usize,
    /// The heading's text.
    text: &'a str,
    /// 3, or 2 for a mislevelled legacy entry.
    level: usize,
    /// 1-based among the file's `###` entries: mycelium's positional N.
    ordinal: usize,
}

struct Spans<'a> {
    /// The last [`MAX_SCANNED_ENTRIES`], in file order.
    kept: VecDeque<EntrySpan<'a>>,
    total: usize,
    mislevelled: usize,
}

impl<'a> Spans<'a> {
    fn push(&mut self, span: EntrySpan<'a>) {
        self.total += 1;
        if self.kept.len() == MAX_SCANNED_ENTRIES {
            self.kept.pop_front();
        }
        self.kept.push_back(span);
    }
}

/// An explicit entry id opening `text` — `D-157`, `D1` (as `D-1`) for
/// `letter` `D` — and the rest after its separator. The dashless form needs
/// a real separator after it (`D1: title`, `D1 — title`) or nothing: "L2
/// cache misses" is a title.
fn explicit_id(text: &str, letter: u8) -> Option<(String, &str)> {
    let t = text.trim_start().trim_start_matches('*');
    let b = t.as_bytes();
    if b.first() != Some(&letter) {
        return None;
    }
    let dashed = b.get(1) == Some(&b'-');
    let from = if dashed { 2 } else { 1 };
    let digits = b[from..].iter().take_while(|c| c.is_ascii_digit()).count();
    if digits == 0 || digits > 4 {
        return None;
    }
    let end = from + digits;
    let rest = &t[end..];
    let separated = rest.is_empty()
        || if dashed {
            rest.starts_with(|c: char| {
                c.is_whitespace() || matches!(c, ':' | '.' | '-' | '–' | '—' | ')' | '*' | ',')
            })
        } else {
            // `D1: title`, `D1. title`, `D1 — title`, `**D1**: title` — but
            // not `L1-norm`, `L2.5 cutoff`, `L2 cache`.
            let r = rest.trim_start_matches('*');
            r.starts_with(':')
                || (r.starts_with('.') && !r[1..].starts_with(|c: char| c.is_ascii_digit()))
                || (rest.starts_with(char::is_whitespace)
                    && rest.trim_start().starts_with([':', '-', '–', '—']))
                || r.trim().is_empty()
        };
    if !separated {
        return None;
    }
    let rest = rest.trim_start_matches(|c: char| {
        c.is_whitespace() || matches!(c, ':' | '.' | '-' | '–' | '—' | ')' | '*')
    });
    Some((format!("{}-{}", letter as char, &t[from..end]), rest))
}

/// A heading's title and its own date: a trailing `(YYYY-MM-DD)` — or a
/// leading one right after the id (`D-47 (2026-07-24) — title`) — moves to
/// the date. A `⛔ …` marker keeps its place at the end, and a date inside
/// it is the marker's, not the entry's.
fn title_and_date(text: &str) -> (String, Option<&str>) {
    let trim = |s: &str| {
        s.trim_matches(|c: char| c.is_whitespace() || matches!(c, ':' | '-' | '–' | '—'))
            .to_owned()
    };
    let text = text.trim_start();
    let (leading, text) = match text.strip_prefix('(') {
        Some(inner)
            if inner.get(..10).is_some_and(|d| is_iso_date(d.as_bytes()))
                && inner.contains(')') =>
        {
            let close = inner.find(')').unwrap_or(inner.len());
            (Some(&inner[..10]), &inner[(close + 1).min(inner.len())..])
        }
        _ => (None, text),
    };
    let (main, marker) = match text.find('⛔') {
        Some(at) => (&text[..at], text[at..].trim()),
        None => (text, ""),
    };
    let main = main.trim_end_matches(|c: char| c.is_whitespace() || matches!(c, '-' | '–' | '—'));
    let (main, trailing) = match trailing_date(main) {
        Some((before, date)) => (before, Some(date)),
        None => (main, None),
    };
    let title = trim(main);
    let date = leading.or(trailing);
    if marker.is_empty() {
        (title, date)
    } else if title.is_empty() {
        (marker.to_owned(), date)
    } else {
        (format!("{title} — {marker}"), date)
    }
}

/// A decisions/learnings heading read two ways: `legacy` as 0.1.3 read it
/// (the fingerprint's input, so an entry's `fp` never moves), and what
/// agents write now — `[date] D-1: title`, `D-157 — title (date)`,
/// `L — title`.
struct LogHeading<'a> {
    legacy: (&'a str, &'a str),
    date: String,
    id: String,
    title: String,
    /// `D-31 addendum (…): …`, `D-38 — REVISION`: a follow-up to the entry
    /// with its id, not another entry with it.
    followup: bool,
}

fn log_heading(text: &str, letter: u8) -> LogHeading<'_> {
    let legacy = split_date_title(text);
    let t = unbold(text);
    let (lead, rest) = leading_date(t);
    let rest = rest.trim_start_matches(|c: char| {
        c.is_whitespace() || matches!(c, ':' | '-' | '–' | '—' | ']')
    });
    let (id, rest) = explicit_id(rest, letter).unwrap_or((String::new(), rest));
    let followup = !id.is_empty() && followup_heading(rest).is_some();
    // `L — title`: the kind's letter as a marker, no number.
    let bare = rest.strip_prefix(letter as char).filter(|r| {
        r.trim_start().starts_with(['—', '–', '-']) && r.starts_with(char::is_whitespace)
    });
    let rest = bare.map_or(rest, |r| {
        r.trim_start()
            .trim_start_matches(['—', '–', '-'])
            .trim_start()
    });
    let (title, trailing) = title_and_date(rest);
    LogHeading {
        legacy,
        date: lead.or(trailing).unwrap_or("").to_owned(),
        id,
        title,
        followup,
    }
}

/// Entry spans of a decisions/learnings log. `### ` at column 1 opens an
/// entry, as in every mycelium parser; a `## ` heading that is DATED or
/// carries an explicit id (`## D-108 — …`) is a mislevelled legacy entry —
/// accepted and counted (mycelium's index doesn't see it). Any other
/// `#`/`##` heading is structure: it closes the open entry and is not one.
fn entry_spans<'a>(doc: &Doc<'a>, letter: u8) -> Spans<'a> {
    let mut spans = Spans {
        kept: VecDeque::new(),
        total: 0,
        mislevelled: 0,
    };
    let mut open: Option<EntrySpan> = None;
    // mycelium's `collect_entries` numbers every column-1 `### ` line, fenced
    // or not: positional ids count the same lines, so `L-40` here is the
    // `L-40` its tools print.
    let mut ordinal = 0usize;
    for i in 0..doc.lines.len() {
        let raw_entry = doc.lines[i].starts_with("### ");
        if raw_entry {
            ordinal += 1;
        }
        let Some((level, text)) = doc.text(i).and_then(heading) else {
            continue;
        };
        let entry = match level {
            3 => true,
            2 if leading_date(unbold(text)).0.is_some()
                || explicit_id(unbold(text), letter).is_some() =>
            {
                spans.mislevelled += 1;
                true
            }
            1 | 2 => false,
            _ => continue,
        };
        if let Some(mut span) = open.take() {
            span.end = i;
            spans.push(span);
        }
        if entry {
            open = Some(EntrySpan {
                start: i,
                end: 0,
                text,
                level,
                ordinal: if level == 3 && raw_entry { ordinal } else { 0 },
            });
        }
    }
    if let Some(mut span) = open {
        span.end = doc.lines.len();
        spans.push(span);
    }
    spans
}

/// One entry of a log, before its kind's fields are read.
struct LogEntry<'a> {
    span: EntrySpan<'a>,
    head: LogHeading<'a>,
    fp: String,
    /// Explicit, else positional when the file has no explicit ids.
    id: String,
}

/// The entries of a decisions/learnings log in file order: fingerprinted
/// (a verbatim duplicate gets `~N`), with ids.
fn log_entries<'a>(
    doc: &Doc<'a>,
    rel: &str,
    kind: &str,
    letter: u8,
    notes: &mut Notes,
) -> Vec<LogEntry<'a>> {
    let Spans {
        kept: spans,
        total,
        mislevelled,
    } = entry_spans(doc, letter);
    if total > spans.len() {
        notes.push(format!(
            "{rel}: {total} entry headings; only the last {} were considered",
            spans.len()
        ));
    }
    if mislevelled > 0 {
        let verb = if mislevelled == 1 {
            "entry uses"
        } else {
            "entries use"
        };
        notes.push(format!(
            "{rel}: {mislevelled} {verb} ## headings (mycelium 0.7 expects ###)"
        ));
    }
    let mut seen: HashMap<String, u32> = HashMap::new();
    let mut entries: Vec<LogEntry> = Vec::with_capacity(spans.len());
    for span in spans {
        let head = log_heading(span.text, letter);
        // A follow-up right after its entry (or that entry's earlier
        // follow-ups) is part of it: the entry's span runs over it.
        if head.followup {
            if let Some(last) = entries
                .last_mut()
                .filter(|e| e.id == head.id && e.span.end == span.start)
            {
                last.span.end = span.end;
                continue;
            }
        }
        let base = fingerprint(kind, head.legacy.0, head.legacy.1);
        let n = seen.entry(base.clone()).or_insert(0);
        *n += 1;
        let fp = if *n == 1 { base } else { format!("{base}~{n}") };
        // A follow-up away from its entry keeps its heading as its title,
        // and no id: the id is the entry's.
        let (id, head) = if head.followup {
            let title = unbold(span.text);
            let title = leading_date(title).1.trim_start_matches([']', ' ']);
            (
                String::new(),
                LogHeading {
                    title: title.to_owned(),
                    ..head
                },
            )
        } else {
            (head.id.clone(), head)
        };
        entries.push(LogEntry { span, head, fp, id });
    }
    // mycelium numbers `###` entries by position; that id means something
    // only where no entry carries its own.
    if entries.iter().all(|e| e.id.is_empty()) {
        for e in entries.iter_mut().filter(|e| e.span.ordinal > 0) {
            e.id = format!("{}-{}", letter as char, e.span.ordinal);
        }
    }
    entries
}

/// `(file order, date, entry)` newest first — undated last, then later in
/// the file first (append order is the only recency an undated entry has)
/// — capped to the newest [`MAX_ENTRIES`].
fn newest_first<T>(mut entries: Vec<(usize, String, T)>, rel: &str, notes: &mut Notes) -> Vec<T> {
    entries.sort_by(|a, b| b.1.cmp(&a.1).then(b.0.cmp(&a.0)));
    if entries.len() > MAX_ENTRIES {
        notes.push(format!(
            "{rel}: showing the newest {MAX_ENTRIES} of {} entries",
            entries.len()
        ));
        entries.truncate(MAX_ENTRIES);
    }
    entries.into_iter().map(|(_, _, entry)| entry).collect()
}

fn line_no(index: usize) -> u32 {
    u32::try_from(index + 1).unwrap_or(u32::MAX)
}

/// What decisions and learnings share: span, refs, cites, the date window
/// for asks.
struct Common {
    span: Span,
    refs: Vec<Ref>,
    cites: Vec<Cite>,
}

fn common(doc: &Doc, rel: &str, span: &EntrySpan, own: &str) -> Common {
    let whole = span.start..span.end;
    let (refs, cites) = refs_and_cites(doc, std::slice::from_ref(&whole), own);
    Common {
        span: span_of(doc, rel, span.start, span.end),
        refs,
        cites,
    }
}

fn parse_decisions(text: &str, rel: &str, notes: &mut Notes) -> Vec<Decision> {
    let doc = Doc::new(text);
    doc.note_unclosed(rel, notes);
    let entries = log_entries(&doc, rel, "decision", b'D', notes)
        .into_iter()
        .enumerate()
        .map(|(order, LogEntry { span, head, fp, id })| {
            let body = span.start + 1..span.end;
            let whole = span.start..span.end;
            let fields = Fields::parse(&doc, std::slice::from_ref(&body), DECISION_FIELDS);
            let date = if head.date.is_empty() {
                fields.date(&["date"]).unwrap_or_default()
            } else {
                head.date
            };
            let decision = fields.text(&["decision", "decision made"]);
            // A bare `### D-52`: the decision's first sentence, else the
            // body's.
            let title = if head.title.is_empty() {
                let lead = if decision.is_empty() {
                    first_prose(&doc, &body, DECISION_FIELDS).unwrap_or_default()
                } else {
                    decision.clone()
                };
                cap_chars(&first_sentence(&lead), MAX_TITLE_CHARS)
            } else {
                cap_text(head.title)
            };
            let Common {
                span: at,
                refs,
                cites,
            } = common(&doc, rel, &span, &id);
            let first = first_lines(&doc, std::slice::from_ref(&body), 3);
            let state = own_state(
                span.text,
                fields.first(&["status"]).unwrap_or(""),
                &first,
                &id,
            );
            let lead: Vec<&str> = std::iter::once(span.text).chain(first).collect();
            let amends = amends_in(&doc, std::slice::from_ref(&whole), &lead, &id);
            let rationale = match fields.text(&["rationale"]) {
                r if r.is_empty() => fields.text(&["why"]),
                r => r,
            };
            let consequences = match fields.text(&["consequences"]) {
                c if c.is_empty() => fields.text(&["consequence"]),
                c => c,
            };
            let entry = Decision {
                fp,
                context: fields.text(&["context"]),
                decision,
                alternatives: fields.items(&[
                    "alternatives considered",
                    "alternatives",
                    "options considered",
                ]),
                rationale,
                consequences,
                tags: fields.tags(),
                line: line_no(span.start),
                id,
                stated: fields.stated(&["status"]),
                span: Some(at),
                refs,
                cites,
                pending: Pending {
                    asks: asks_in(&doc, rel, std::slice::from_ref(&whole))
                        .into_iter()
                        .map(|(text, span)| (text, span, day_number(&date)))
                        .collect(),
                    day: day_number(&date),
                    own_state: state.is_some(),
                    resolved: false,
                    off_index: span.level == 2,
                },
                state,
                amends,
                title,
                date: date.clone(),
            };
            (order, date, entry)
        })
        .collect();
    newest_first(entries, rel, notes)
}

fn parse_learnings(text: &str, rel: &str, notes: &mut Notes) -> Vec<Learning> {
    let doc = Doc::new(text);
    doc.note_unclosed(rel, notes);
    let entries = log_entries(&doc, rel, "learning", b'L', notes)
        .into_iter()
        .enumerate()
        .map(|(order, LogEntry { span, head, fp, id })| {
            let body = span.start + 1..span.end;
            let fields = Fields::parse(&doc, std::slice::from_ref(&body), LEARNING_FIELDS);
            let date = if head.date.is_empty() {
                fields.date(&["date"]).unwrap_or_default()
            } else {
                head.date
            };
            let category = fields
                .first(&["category"])
                .map_or_else(|| "other".to_owned(), normalize_category);
            let or_else = |first: String, then: &[&[&str]]| {
                then.iter().fold(first, |got, names| {
                    if got.is_empty() {
                        fields.text(names)
                    } else {
                        got
                    }
                })
            };
            let Common {
                span: at,
                refs,
                cites,
            } = common(&doc, rel, &span, &id);
            let entry = Learning {
                fp,
                title: cap_text(head.title),
                category,
                what: or_else(fields.text(&["what happened", "what"]), &[&["symptom"]]),
                why: fields.text(&["why it matters", "why"]),
                resolution: or_else(
                    fields.text(&["resolution", "fix"]),
                    &[
                        &["how to apply"],
                        &["generalisable rule", "generalizable rule"],
                    ],
                ),
                tags: fields.tags(),
                line: line_no(span.start),
                id,
                span: Some(at),
                refs,
                cites,
                pending: Pending {
                    off_index: span.level == 2,
                    ..Pending::default()
                },
                date: date.clone(),
            };
            (order, date, entry)
        })
        .collect();
    newest_first(entries, rel, notes)
}

// ---------------------------------------------------------------------------
// Findings
// ---------------------------------------------------------------------------

/// `F-NNN` at the start of a heading: `(id, numeric id, the rest)`. A joint
/// heading (`F-020/F-021 addendum: …`) is the first id's.
fn finding_id(text: &str) -> Option<(String, u64, &str)> {
    let t = text.trim().trim_start_matches('*');
    let rest = t.strip_prefix("F-")?;
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 {
        return None;
    }
    let is_sep =
        |c: char| c.is_whitespace() || matches!(c, ':' | '.' | '-' | '–' | '—' | ')' | '*');
    let mut after = &rest[digits..];
    while let Some(more) = after.strip_prefix("/F-") {
        let n = more.bytes().take_while(u8::is_ascii_digit).count();
        if n == 0 {
            return None;
        }
        after = &more[n..];
    }
    if !(after.is_empty() || after.starts_with(is_sep)) {
        return None;
    }
    let num = rest[..digits].parse().unwrap_or(u64::MAX);
    let claim = after
        .trim_start_matches(is_sep)
        .trim_end_matches('*')
        .trim();
    Some((format!("F-{}", &rest[..digits]), num, claim))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Subsection {
    Other,
    Ledger,
    Questions,
}

fn subsection(text: &str) -> Subsection {
    let lower = text.to_lowercase();
    if lower.starts_with("evidence") {
        Subsection::Ledger
    } else if lower.starts_with("open question") {
        Subsection::Questions
    } else {
        Subsection::Other
    }
}

/// The words that open a follow-up heading after `F-NNN`, and their kind.
const FOLLOWUP_WORDS: &[(&str, &str)] = &[
    ("addendum", "addendum"),
    ("addenda", "addendum"),
    ("correction", "correction"),
    ("corrected", "correction"),
    ("erratum", "correction"),
    ("resolution", "resolution"),
    ("resolved", "resolution"),
    ("update", "update"),
    ("updated", "update"),
    ("revision", "update"),
    ("revised", "update"),
    ("reprocess", "update"),
];

/// A follow-up heading's parts.
#[derive(Clone)]
struct Followup<'t> {
    /// addendum | correction | resolution | update
    kind: &'static str,
    /// The heading's word and qualifier: "Addendum (2)", "CORRECTION
    /// (2026-07-23)", "Reprocess round 1", "Update".
    label: String,
    /// After the separator; may be empty.
    title: &'t str,
}

/// What follows `F-NNN` in a follow-up heading — `addendum: title`,
/// `Addendum (2) — title`, `CORRECTION (2026-07-23): title`, `RESOLVED`,
/// `reprocess round 1 (date): title` — as its kind, label and title. The
/// word may carry one qualifier (parenthesized, or a token without
/// letters), then only a separator or nothing: a claim that merely opens
/// with the word ("Addendum to protocol v2 improves yield", "Correction of
/// batch labels…") is a finding.
fn followup_heading(rest: &str) -> Option<Followup<'_>> {
    let (word, kind) = FOLLOWUP_WORDS
        .iter()
        .filter(|(w, _)| {
            rest.get(..w.len())
                .is_some_and(|head| head.eq_ignore_ascii_case(w))
        })
        .max_by_key(|(w, _)| w.len())
        .map(|&(w, kind)| (w.len(), kind))?;
    let mut word_end = word;
    // `reprocess round 1`: the round is part of the word.
    if rest[..word].eq_ignore_ascii_case("reprocess") {
        let t = rest[word..].trim_start();
        if t.get(..5).is_some_and(|r| r.eq_ignore_ascii_case("round")) {
            let n = t[5..].trim_start();
            let digits = n.bytes().take_while(u8::is_ascii_digit).count();
            if digits > 0 && t[5..].starts_with(char::is_whitespace) {
                word_end = rest.len() - (n.len() - digits);
            }
        }
    }
    // A separator and what follows it, or nothing at all.
    fn title(t: &str) -> Option<&str> {
        let t = t.trim_start().trim_start_matches('*').trim_start();
        if t.is_empty() {
            return Some(t);
        }
        t.strip_prefix([':', '—', '–']).or_else(|| {
            t.strip_prefix('-')
                .filter(|r| r.starts_with(char::is_whitespace))
        })
    }
    let after = &rest[word_end..];
    let (qualifier, title) = match title(after) {
        Some(title) => ("", title),
        None => {
            let t = after.trim_start();
            let (qualifier, tail) = match t.strip_prefix('(') {
                Some(inner) => {
                    let close = inner.find(')')?;
                    (&t[..close + 2], &inner[close + 1..])
                }
                None => {
                    let end = t
                        .find(|c: char| c.is_whitespace() || matches!(c, ':' | '—' | '–' | '*'))
                        .unwrap_or(t.len());
                    if t[..end].chars().any(char::is_alphabetic) {
                        return None;
                    }
                    t.split_at(end)
                }
            };
            (qualifier, title(tail)?)
        }
    };
    let mut label = rest[..word_end].to_owned();
    label[..1].make_ascii_uppercase();
    if !qualifier.is_empty() {
        label.push(' ');
        label.push_str(qualifier);
    }
    Some(Followup {
        kind,
        label,
        title: title.trim().trim_matches('*').trim(),
    })
}

/// The first `YYYY-MM-DD` anywhere in `s`.
fn first_iso(s: &str) -> Option<&str> {
    let b = s.as_bytes();
    (0..b.len().saturating_sub(9))
        .filter(|&i| s.is_char_boundary(i) && (i == 0 || !b[i - 1].is_ascii_digit()))
        .find(|&i| is_iso_date(&b[i..i + 10]) && !b.get(i + 10).is_some_and(u8::is_ascii_digit))
        .map(|i| &s[i..i + 10])
}

/// Where the section a heading opens ends: at the next heading above its
/// level, or at its level unless that is one of a `###` (or deeper)
/// heading's own subsections (Evidence, Open questions) — never past
/// `limit`.
fn section_end(doc: &Doc, line: usize, level: usize, limit: usize) -> usize {
    (line + 1..limit)
        .find(|&i| {
            doc.text(i).and_then(heading).is_some_and(|(l, text)| {
                l < level || (l == level && (l <= 2 || subsection(text) == Subsection::Other))
            })
        })
        .unwrap_or(limit)
}

/// An `F-` heading in a topic file.
struct Head<'t> {
    line: usize,
    level: usize,
    /// The heading's text.
    text: &'t str,
    /// The next `F-` heading's line (or EOF): no span passes it.
    limit: usize,
    /// A finding's: the next finding's line (or EOF). Its span runs over
    /// the addenda before that, which are cut out of it.
    reach: usize,
    id: String,
    num: u64,
    claim: &'t str,
    /// A follow-up's kind, label and title.
    followup: Option<Followup<'t>>,
}

/// A follow-up's heading and lines, for [`parse_finding`].
struct AddendumSpan<'t> {
    followup: Followup<'t>,
    /// 0-based heading line.
    line: usize,
    /// Its lines after the heading.
    body: Range<usize>,
}

/// An addendum's text as written, less what the finding takes — its Status
/// and Tags lines, its Evidence and Open questions subsections — and
/// comments. Its own sub-headings become bold lines.
fn addendum_text(doc: &Doc, body: &Range<usize>) -> String {
    let mut lines: Vec<Cow<str>> = Vec::new();
    let mut section = Subsection::Other;
    let mut tag_list = false;
    for i in body.clone() {
        let line = doc.lines[i];
        match doc.marks[i] {
            Mark::Comment => continue,
            Mark::Code => {
                if section == Subsection::Other {
                    lines.push(Cow::Borrowed(line));
                }
                continue;
            }
            Mark::Text => {}
        }
        if let Some((_, text)) = heading(line) {
            section = subsection(text);
            tag_list = false;
            if section == Subsection::Other && !text.is_empty() {
                lines.extend([
                    Cow::Borrowed(""),
                    Cow::Owned(format!("**{text}**")),
                    Cow::Borrowed(""),
                ]);
            }
            continue;
        }
        if is_thematic_break(line) {
            section = Subsection::Other;
            tag_list = false;
            continue;
        }
        let field = field_line(line, FINDING_FIELDS);
        if field.is_some() {
            tag_list = false;
        }
        match field {
            Some((label, _)) if label == "status" => continue,
            // `**Tags**:` over a bullet list: the bullets are the tags.
            Some((label, value)) if label == "tags" => {
                tag_list = value.is_empty();
                continue;
            }
            _ => {}
        }
        if tag_list && (line.trim().is_empty() || list_marker(line.trim_start()).is_some()) {
            continue;
        }
        tag_list = false;
        if section != Subsection::Other {
            continue;
        }
        let stripped = strip_inline_comments(line);
        if stripped.trim().is_empty() && !line.trim().is_empty() {
            continue;
        }
        lines.push(stripped);
    }
    tidy(&lines)
}

/// A topic file's findings — at most `budget` of them, the lowest ids, each
/// with its follow-ups — and how many the file has in all (for the cap
/// warning). `None` for a file with no findings (or none left in the
/// budget).
fn parse_topic(
    text: &str,
    rel: &str,
    slug: &str,
    budget: usize,
    notes: &mut Notes,
) -> (Option<Topic>, usize) {
    let (doc, front) = Doc::with_frontmatter(text);
    doc.note_unclosed(rel, notes);
    let meta = |key: &str| {
        front
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
            .filter(|v| !is_placeholder(v))
    };

    let eof = doc.lines.len();
    let mut heads: Vec<Head> = Vec::new();
    let mut total = 0usize;
    let mut open_finding: Option<usize> = None;
    let mut title: Option<&str> = None;
    // The level of the latest finding (not follow-up) heading with each id:
    // a deeper heading with the id is a follow-up even without a keyword
    // (`### F-031 recount VALIDATED on sample-a …` under `## F-031: …`).
    let mut finding_levels: HashMap<String, usize> = HashMap::new();
    for i in 0..eof {
        let Some((level, text)) = doc.text(i).and_then(heading) else {
            continue;
        };
        if level == 1 && title.is_none() {
            title = Some(text);
        }
        let Some((id, num, claim)) = finding_id(text) else {
            continue;
        };
        let followup = followup_heading(claim).or_else(|| {
            finding_levels
                .get(id.as_str())
                .is_some_and(|&at| level > at)
                .then(|| Followup {
                    kind: "update",
                    label: "Update".to_owned(),
                    title: claim,
                })
        });
        if !(matches!(level, 2 | 3) || (level == 4 && followup.is_some())) {
            continue;
        }
        total += 1;
        if let Some(last) = heads.last_mut().filter(|h| h.limit == eof) {
            last.limit = i;
        }
        if followup.is_none() {
            if let Some(k) = open_finding.take() {
                heads[k].reach = i;
            }
            if heads.len() < MAX_SCANNED_ENTRIES {
                finding_levels.insert(id.clone(), level);
            }
        }
        if heads.len() < MAX_SCANNED_ENTRIES {
            if followup.is_none() {
                open_finding = Some(heads.len());
            }
            heads.push(Head {
                line: i,
                level,
                text,
                limit: eof,
                reach: eof,
                id,
                num,
                claim,
                followup,
            });
        }
    }
    if total > MAX_SCANNED_ENTRIES {
        notes.push(format!(
            "{rel}: {total} finding headings; only the first {MAX_SCANNED_ENTRIES} were considered"
        ));
    }

    // Each finding with its follow-ups, as indexes into `heads`. A follow-up
    // belongs to the latest finding above it with its id; with none, it
    // leads until its finding appears (written below it) or the file ends
    // (the finding is elsewhere, or nowhere). A reused id is two findings,
    // told apart by their keys (`slug/F-038`, `slug/F-038~2`).
    struct Group {
        finding: Option<usize>,
        /// Never empty without a finding.
        addenda: Vec<usize>,
        /// 1 for the first group with its id in this file, 2 for the next…
        nth: usize,
    }
    impl Group {
        /// The heading the finding is shown at.
        fn lead(&self) -> usize {
            self.finding.unwrap_or_else(|| self.addenda[0])
        }
    }
    let mut groups: Vec<Group> = Vec::new();
    let mut latest: HashMap<&str, usize> = HashMap::new();
    let mut per_id: HashMap<&str, usize> = HashMap::new();
    for (k, head) in heads.iter().enumerate() {
        let group = latest.get(head.id.as_str()).map(|&g| &mut groups[g]);
        match (head.followup.is_some(), group) {
            (true, Some(group)) => group.addenda.push(k),
            (false, Some(group)) if group.finding.is_none() => group.finding = Some(k),
            (followup, _) => {
                let nth = per_id.entry(&head.id).or_insert(0);
                *nth += 1;
                latest.insert(&head.id, groups.len());
                groups.push(Group {
                    finding: (!followup).then_some(k),
                    addenda: if followup { vec![k] } else { Vec::new() },
                    nth: *nth,
                });
            }
        }
    }
    for group in groups.iter().filter(|g| g.finding.is_none()) {
        let head = &heads[group.lead()];
        notes.push(format!(
            "{rel}: the {id} addendum at line {} has no {id} finding in this file; shown on its own",
            line_no(head.line),
            id = head.id
        ));
    }
    // Headings past the scan cap count as findings, unexamined.
    let found = groups.len() + (total - heads.len());
    // Stable: a reused id keeps file order.
    groups.sort_by_key(|g| heads[g.lead()].num);
    groups.truncate(budget);
    if groups.is_empty() {
        return (None, found);
    }

    let last_updated = meta("last_updated")
        .filter(|v| v.as_bytes().get(..10).is_some_and(is_iso_date))
        .map(|v| v[..10].to_owned());
    let span = |head: &Head| head.line + 1..section_end(&doc, head.line, head.level, head.limit);
    let cx = TopicCx {
        doc: &doc,
        rel,
        last_updated: last_updated.as_deref(),
    };
    let findings = groups
        .iter()
        .map(|group| {
            let mut own = Vec::new();
            let (lead, end) = match group.finding {
                Some(k) => {
                    // A `## F-` finding runs to the next `#`/`##` heading; a
                    // `### F-` one also stops at a `###` that isn't one of its
                    // subsections. Follow-ups written inside it (of any id)
                    // are cut out.
                    let head = &heads[k];
                    let end = section_end(&doc, head.line, head.level, head.reach);
                    let mut from = head.line + 1;
                    let inside = heads[k + 1..]
                        .iter()
                        .take_while(|h| h.followup.is_some() && h.line < end);
                    for inner in inside {
                        own.push(from..inner.line);
                        from = span(inner).end.min(end);
                    }
                    own.push(from..end);
                    (head, end)
                }
                // No finding in the file: the first follow-up stands in for
                // it, its heading the claim.
                None => {
                    let head = &heads[group.lead()];
                    (head, span(head).end)
                }
            };
            let parts: Vec<AddendumSpan> = group
                .addenda
                .iter()
                .map(|&k| {
                    let head = &heads[k];
                    AddendumSpan {
                        followup: head.followup.clone().expect("a follow-up heading"),
                        line: head.line,
                        body: span(head),
                    }
                })
                .collect();
            let key = if group.nth == 1 {
                format!("{slug}/{}", lead.id)
            } else {
                format!("{slug}/{}~{}", lead.id, group.nth)
            };
            let whole = lead.line..end;
            let lead_is_finding = group.finding.is_some();
            parse_finding(&cx, &own, &parts, lead, lead_is_finding, key, whole, notes)
        })
        .collect();

    let description = meta("description")
        .or_else(|| meta("topic"))
        .or(title.filter(|t| !is_placeholder(t)))
        .unwrap_or("");
    let topic = Topic {
        slug: cap_text(slug.to_owned()),
        description: cap_text(description.to_owned()),
        path: rel.to_owned(),
        findings,
        date: last_updated.unwrap_or_default(),
    };
    (Some(topic), found)
}

/// What every finding of a topic file shares.
struct TopicCx<'d, 'a> {
    doc: &'d Doc<'a>,
    rel: &'d str,
    last_updated: Option<&'d str>,
}

/// A finding from its own lines (`own`: its span less the follow-ups
/// written inside it) and its follow-ups, in file order. Claim, status and
/// implications are the finding's own; each follow-up adds its tags,
/// evidence rows and open questions — and its status, when the finding
/// states none (the newest one stated wins) — and is listed with its text.
#[allow(clippy::too_many_arguments)]
fn parse_finding(
    cx: &TopicCx,
    own: &[Range<usize>],
    addenda: &[AddendumSpan],
    lead: &Head,
    lead_is_finding: bool,
    key: String,
    whole: Range<usize>,
    notes: &mut Notes,
) -> Finding {
    let doc = cx.doc;
    let id = lead.id.as_str();
    let fields = Fields::parse(doc, own, FINDING_FIELDS);
    // In file order across the finding and its follow-ups: ledgers append,
    // and the newest rows are the ones kept.
    let mut spans: Vec<&Range<usize>> = own.iter().chain(addenda.iter().map(|a| &a.body)).collect();
    spans.sort_by_key(|span| span.start);
    let mut ledger_lines = Vec::new();
    let mut question_lines = Vec::new();
    for span in spans {
        let mut section = Subsection::Other;
        for i in span.clone() {
            let Some(line) = doc.text(i) else {
                continue;
            };
            if let Some((_, text)) = heading(line) {
                section = subsection(text);
            } else if is_thematic_break(line) {
                section = Subsection::Other;
            } else if section == Subsection::Ledger {
                ledger_lines.push(line);
            } else if section == Subsection::Questions {
                question_lines.push(line);
            }
        }
    }

    let ledger = parse_ledger(&ledger_lines);
    let updated = ledger
        .newest
        .or_else(|| cx.last_updated.map(str::to_owned))
        .unwrap_or_default();
    if ledger.total > ledger.rows.len() {
        notes.push(format!(
            "{id}: showing the newest {} of {} evidence rows",
            ledger.rows.len(),
            ledger.total
        ));
    }

    let open: Vec<ListItem> = list_items(question_lines)
        .into_iter()
        .filter(|q| !q.done && !is_placeholder(&q.text) && !says_none(&q.text))
        .collect();
    if open.len() > MAX_QUESTIONS_PER_FINDING {
        notes.push(format!(
            "{id}: showing the first {MAX_QUESTIONS_PER_FINDING} of {} open questions",
            open.len()
        ));
    }
    let questions = live_items(open, MAX_QUESTIONS_PER_FINDING);

    let (heading_claim, heading_date) = title_and_date(lead.claim);
    let claim_field = fields.text(&["claim"]);
    let (claim, statement) = if is_placeholder(&heading_claim) {
        (claim_field, String::new())
    } else {
        let statement = if plain(&claim_field) == plain(&heading_claim) {
            String::new()
        } else {
            claim_field
        };
        (cap_text(heading_claim), statement)
    };
    let date = heading_date
        .map(str::to_owned)
        .or_else(|| fields.date(&["date"]))
        .unwrap_or_default();
    let states_own = fields.get(&["status"]).is_some();
    let mut status = fields
        .first(&["status"])
        .map_or_else(|| "unknown".to_owned(), normalize_status);
    let known_status = status != "unknown";
    let mut stated = fields.stated(&["status"]);
    // Where `status` came from when a follow-up gave it: `stated` then
    // quotes that same Status.
    let mut status_from_followup = false;
    let mut tags = fields.tags();
    let mut listed = Vec::with_capacity(addenda.len().min(MAX_ADDENDA));
    let own_day = day_number(&date);
    let mut day = own_day;
    let mut followup_days: Vec<Option<i64>> = Vec::with_capacity(addenda.len());
    for (n, addendum) in addenda.iter().enumerate() {
        let its = Fields::parse(doc, std::slice::from_ref(&addendum.body), FINDING_FIELDS);
        let its_stated = its.stated(&["status"]);
        if let Some(said) = its
            .first(&["status"])
            .map(normalize_status)
            .filter(|s| !known_status && s != "unknown")
        {
            status = said;
            stated.clone_from(&its_stated);
            status_from_followup = true;
        } else if !states_own && !status_from_followup && !its_stated.is_empty() {
            stated.clone_from(&its_stated);
        }
        for tag in its.tags() {
            push_tag(&tag, &mut tags);
        }
        let followup = &addendum.followup;
        let its_date = first_iso(&followup.label)
            .or_else(|| paren_date(followup.title))
            .map(str::to_owned)
            .or_else(|| its.date(&["date"]))
            .unwrap_or_default();
        let its_day = day_number(&its_date);
        if let Some(d) = its_day {
            day = Some(day.map_or(d, |had| had.max(d)));
        }
        followup_days.push(its_day);
        // The newest are listed, since they append.
        if n + MAX_ADDENDA >= addenda.len() {
            listed.push(Addendum {
                label: cap_text(followup.label.clone()),
                title: cap_text(followup.title.to_owned()),
                text: cap_text(addendum_text(doc, &addendum.body)),
                line: line_no(addendum.line),
                kind: followup.kind,
                date: its_date,
                stated: its_stated,
                span: Some(span_of(doc, cx.rel, addendum.line, addendum.body.end)),
            });
        }
    }
    if addenda.len() > MAX_ADDENDA {
        notes.push(format!(
            "{id}: showing the newest {MAX_ADDENDA} of {} addenda",
            addenda.len()
        ));
    }

    // Everything the finding wrote: its heading, own lines and follow-ups.
    let mut scan: Vec<Range<usize>> = Vec::new();
    if lead_is_finding {
        scan.push(lead.line..lead.line + 1);
        scan.extend(own.iter().cloned());
    }
    scan.extend(addenda.iter().map(|a| a.line..a.body.end));
    scan.sort_by_key(|r| r.start);
    let (refs, cites) = refs_and_cites(doc, &scan, id);
    let first = first_lines(doc, own, 3);
    let state = own_state(
        lead.text,
        fields.first(&["status"]).unwrap_or(""),
        &first,
        id,
    );
    let lead_lines: Vec<&str> = std::iter::once(lead.text).chain(first).collect();
    let amends = amends_in(doc, &scan, &lead_lines, id);
    Finding {
        id: id.to_owned(),
        claim,
        status,
        implications: fields.text(&["implications", "implication"]),
        tags,
        ledger: ledger.rows.into(),
        questions,
        addenda: listed,
        line: line_no(lead.line),
        updated,
        key,
        stated,
        date,
        span: Some(span_of(doc, cx.rel, whole.start, whole.end)),
        refs,
        cites,
        pending: Pending {
            asks: {
                // The finding's own lines at its own date (an undated one at
                // the newest it has); each follow-up's at its own.
                let body_day = own_day.or(day);
                let mut body: Vec<Range<usize>> = Vec::new();
                if lead_is_finding {
                    body.push(lead.line..lead.line + 1);
                    body.extend(own.iter().cloned());
                }
                let mut asks: Vec<(String, Span, Option<i64>)> = asks_in(doc, cx.rel, &body)
                    .into_iter()
                    .map(|(text, span)| (text, span, body_day))
                    .collect();
                for (a, its_day) in addenda.iter().zip(&followup_days) {
                    let range = a.line..a.body.end;
                    asks.extend(
                        asks_in(doc, cx.rel, std::slice::from_ref(&range))
                            .into_iter()
                            .map(|(text, span)| (text, span, its_day.or(body_day))),
                    );
                }
                asks.truncate(MAX_ASKS_PER_ENTRY);
                asks
            },
            day,
            own_state: state.is_some(),
            resolved: addenda
                .last()
                .is_some_and(|a| a.followup.kind == "resolution"),
            off_index: false,
        },
        state,
        amends,
        statement,
    }
}

/// Column indexes for date, run, dataset, project, result, direction.
type LedgerColumns = [Option<usize>; 6];

/// mycelium's column order, for a table with no recognizable header.
const LEDGER_POSITIONAL: LedgerColumns = [Some(0), Some(1), Some(2), Some(3), Some(4), Some(5)];

fn ledger_columns(header: &[String]) -> Option<LedgerColumns> {
    let mut cols: LedgerColumns = [None; 6];
    for (i, cell) in header.iter().enumerate() {
        let name = cell.to_lowercase();
        let slot = if name.contains("date") {
            0
        } else if name.contains("run") || name.contains("session") {
            1
        } else if name.contains("dataset") || name == "data" {
            2
        } else if name.contains("project") {
            3
        } else if name.contains("result") || name.contains("observ") {
            4
        } else if name.contains("direction") {
            5
        } else {
            continue;
        };
        cols[slot].get_or_insert(i);
    }
    cols.iter().any(Option::is_some).then_some(cols)
}

struct Ledger {
    /// The newest [`MAX_LEDGER_ROWS`] (ledgers append), in file order.
    rows: VecDeque<LedgerRow>,
    total: usize,
    /// Newest ISO date across ALL rows, kept or not.
    newest: Option<String>,
}

/// An Evidence Ledger table, streamed: only the kept rows are ever built.
/// Columns are found by header name; mycelium's order is the fallback.
fn parse_ledger(lines: &[&str]) -> Ledger {
    let table: Vec<&str> = lines
        .iter()
        .copied()
        .filter(|l| l.trim_start().starts_with('|'))
        .collect();
    let mut ledger = Ledger {
        rows: VecDeque::new(),
        total: 0,
        newest: None,
    };
    let mut cols = LEDGER_POSITIONAL;
    for (k, line) in table.iter().enumerate() {
        let Some(row) = table_cells(line) else {
            continue;
        };
        if is_separator(&row) {
            continue;
        }
        let header = table
            .get(k + 1)
            .and_then(|next| table_cells(next))
            .is_some_and(|next| is_separator(&next));
        if header {
            cols = ledger_columns(&row).unwrap_or(LEDGER_POSITIONAL);
            continue;
        }
        let cell = |slot: usize| {
            cols[slot]
                .and_then(|i| row.get(i))
                .map_or_else(String::new, |c| cell_text(c))
        };
        let date = cell(0);
        // mycelium's template row (`| YYYY-MM-DD | {session-id} | …`).
        if date.eq_ignore_ascii_case("yyyy-mm-dd") || date.starts_with('{') {
            continue;
        }
        if row.iter().all(|c| c.trim().is_empty()) {
            continue;
        }
        ledger.total += 1;
        if let Some(day) = date.get(..10).filter(|d| is_iso_date(d.as_bytes())) {
            if ledger.newest.as_deref().is_none_or(|newest| day > newest) {
                ledger.newest = Some(day.to_owned());
            }
        }
        let direction = cols[5]
            .and_then(|i| row.get(i))
            .map_or_else(|| "unknown".to_owned(), |c| normalize_direction(c));
        if ledger.rows.len() == MAX_LEDGER_ROWS {
            ledger.rows.pop_front();
        }
        ledger.rows.push_back(LedgerRow {
            run: cell(1),
            dataset: cell(2),
            project: cell(3),
            result: cell(4),
            date,
            direction,
        });
    }
    ledger
}

// ---------------------------------------------------------------------------
// Handoff
// ---------------------------------------------------------------------------

/// Section index for a handoff heading, across the schemas agents write:
/// the five-section one, `finalize_handoff.py`'s other one (Current State /
/// What Was Done / Key Decisions / Next Steps / Relevant Files) and numbered
/// headings (`## 1. Goal`, `## 2. Done`, `## 3. In flight`, `## 4. Next`).
/// Goal and In flight are the current state. "Relevant Files", "Rules" and
/// other sections have no slot (the view renders the file itself).
fn handoff_section(text: &str) -> Option<usize> {
    let name = collapse_ws(&plain(text).to_lowercase().replace('&', "and"));
    let name = name.trim_end_matches(':');
    // `1. Goal`, `2) Done`
    let digits = name.bytes().take_while(u8::is_ascii_digit).count();
    let name = match name[digits..].strip_prefix(['.', ')']) {
        Some(rest) if digits > 0 => rest.trim_start(),
        _ => name,
    };
    match name {
        "what was worked on" | "what was done" | "what we worked on" | "done" | "worked on" => {
            Some(0)
        }
        "key decisions made" | "key decisions" | "decisions made" | "decisions" => Some(1),
        "blockers and surprises" | "blockers" => Some(2),
        "current state" | "goal" | "in flight" | "in-flight" => Some(3),
        "next steps" | "next" => Some(4),
        _ => None,
    }
}

/// mycelium's `finalize_handoff._STALE_STOP_LINE`, word-level: lifecycle
/// chatter ("Stop hook finalization pending", "attempting a natural stop")
/// that the finalizer strips on acceptance. An in-flight run handoff hasn't
/// been through it yet.
fn is_stale_stop_line(line: &str) -> bool {
    let lower = line.to_lowercase();
    let words: Vec<&str> = lower
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    // Only the FIRST lead can matter (any later one's tail is a suffix of
    // its tail), which keeps this linear on a hostile one-line file.
    let lead = words.windows(2).position(|pair| {
        matches!(pair, ["natural", "stop"])
            || matches!(pair, ["stop", "hook" | "finalization" | "attempt"])
    });
    let lifecycle = lead.is_some_and(|at| {
        let tail = &words[at + 2..];
        tail.iter().enumerate().any(|(j, w)| {
            matches!(*w, "pending" | "remain" | "remains" | "attempt")
                || (*w == "not" && tail.get(j + 1) == Some(&"yet"))
        })
    });
    lifecycle
        || words
            .iter()
            .position(|w| matches!(*w, "attempt" | "attempting"))
            .is_some_and(|at| words[at + 1..].contains(&"stop"))
}

/// The handoff minus mycelium's lifecycle-status block and stale Stop lines —
/// the body `finalize_handoff.clean_handoff_body` would publish.
fn clean_handoff(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let closed = text.contains(STATUS_END);
    let mut in_status = false;
    for line in text.split('\n') {
        if closed && !in_status && line.contains(STATUS_BEGIN) {
            in_status = true;
        }
        if in_status {
            in_status = !line.contains(STATUS_END);
            continue;
        }
        if is_stale_stop_line(line) {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// What the handoff read yields: the chosen handoff, its asks, and the
/// Tidy up row when the Stop hook's stub hides a hand-written one.
#[derive(Default)]
struct HandoffRead {
    left: Option<LeftOff>,
    asks: Vec<AskItem>,
    stub: Option<Tidy>,
}

/// Whether a handoff is the Stop hook's deterministic fallback.
fn is_stub(text: &str) -> bool {
    text.to_lowercase().contains(STUB_LINE)
}

/// The newest hand-written handoff by mtime that says something — the
/// shared file wins only an exact tie — plus every handoff found, newest
/// first. The Stop hook's fallback stub is chosen only when no hand-written
/// handoff among those read says anything: it is written at a session's end,
/// so it is often the newest while the agent's own handoff is the real one.
/// At most three are read to find one, and the shared file once more for
/// the stub check.
fn read_handoff(
    fs: &impl Fs,
    handoffs: &[Source],
    budget: &mut Budget,
    notes: &mut Notes,
) -> HandoffRead {
    const SHARED: &str = ".mycelium/last-session.md";
    let mut out = HandoffRead::default();
    let mut order: Vec<&Source> = handoffs.iter().collect();
    order.sort_by(|a, b| {
        (b.stat.mtime_ms, b.rel == SHARED, &b.rel).cmp(&(a.stat.mtime_ms, a.rel == SHARED, &a.rel))
    });
    let sources: Vec<HandoffSource> = order
        .iter()
        .map(|s| {
            let (session_id, host) = match &s.kind {
                SourceKind::Handoff { session_id, host } => (session_id.clone(), host.clone()),
                _ => (None, None),
            };
            HandoffSource {
                path: s.rel.clone(),
                written_ms: s.stat.mtime_ms,
                session_id,
                host,
            }
        })
        .collect();
    let mut texts: Vec<(&str, String)> = Vec::new();
    // (source, its text's index in `texts`, what it parsed to).
    let mut chosen: Option<(&Source, usize, LeftOff)> = None;
    let mut stub: Option<(&Source, usize, LeftOff)> = None;
    for source in order.iter().take(3) {
        let Some(text) = budget.read(fs, source, notes) else {
            continue;
        };
        let parsed = parse_handoff(&text, source, notes);
        let is_stub_text = is_stub(&text);
        texts.push((&source.rel, text));
        let Some(left) = parsed else {
            continue;
        };
        let at = texts.len() - 1;
        if is_stub_text {
            if stub.is_none() {
                stub = Some((source, at, left));
            }
            continue;
        }
        chosen = Some((source, at, left));
        break;
    }
    if let Some((source, at, mut left)) = chosen.or(stub) {
        if let SourceKind::Handoff { session_id, host } = &source.kind {
            left.session_id.clone_from(session_id);
            left.host.clone_from(host);
        }
        let doc = Doc::new(&texts[at].1);
        left.span = Some(span_of(&doc, &source.rel, 0, doc.lines.len()));
        let date = date_of_ms(source.stat.mtime_ms);
        let all = 0..doc.lines.len();
        let asks = asks_in(&doc, &source.rel, std::slice::from_ref(&all));
        out.asks = asks
            .into_iter()
            .map(|(text, span)| AskItem {
                text,
                date: date.clone(),
                source: AskSource {
                    kind: "handoff",
                    id: left.session_id.clone().unwrap_or_default(),
                    key: source.rel.clone(),
                },
                span,
            })
            .collect();
        left.sources.clone_from(&sources);
        out.left = Some(left);
    }

    // The shared handoff is the stub while the newest run handoff is
    // hand-written: the Stop hook replaced what an agent wrote.
    let shared = order.iter().find(|s| s.rel == SHARED);
    let run = order.iter().find(|s| s.rel != SHARED);
    if let (Some(shared), Some(run)) = (shared, run) {
        let mut text_of = |source: &Source| -> Option<bool> {
            if let Some((_, text)) = texts.iter().find(|(rel, _)| *rel == source.rel) {
                return Some(is_stub(text));
            }
            let text = budget.read(fs, source, notes)?;
            Some(is_stub(&text))
        };
        if text_of(shared) == Some(true) && text_of(run) == Some(false) {
            let newer = if run.stat.mtime_ms > shared.stat.mtime_ms {
                "newer "
            } else {
                ""
            };
            out.stub = Some(Tidy {
                kind: "handoff-stub",
                text: format!(
                    "The shared handoff {SHARED} is the Stop hook's fallback stub, while a {newer}hand-written handoff exists at {}.",
                    run.rel
                ),
                refs: Vec::new(),
                ask: cap_bytes(
                    format!(
                        "{SHARED} holds only the Stop hook's fallback text (\"Completed the session work recorded in the finalized session log…\"), while {} is a hand-written handoff. Read both, then rewrite {SHARED} as a real handoff — What was worked on, Key decisions, Blockers, Current state, Next steps — carrying over what the hand-written one says is done, in flight and next. Keep {} as it is.",
                        run.rel, run.rel
                    ),
                    MAX_ASK_BYTES,
                ),
            });
        }
    }
    out
}

fn parse_handoff(text: &str, source: &Source, notes: &mut Notes) -> Option<LeftOff> {
    let cleaned = clean_handoff(text);
    let doc = Doc::new(&cleaned);
    doc.note_unclosed(&source.rel, notes);
    // A known section heading opens its slot (a repeat appends to it); any
    // other heading at or above its level closes it; deeper ones are content.
    let mut sections: [Vec<&str>; 5] = Default::default();
    let mut current: Option<(usize, usize)> = None;
    for (i, &line) in doc.lines.iter().enumerate() {
        match doc.marks[i] {
            Mark::Comment => continue,
            Mark::Code => {}
            Mark::Text => {
                if let Some((level, text)) = heading(line) {
                    if let Some(slot) = handoff_section(text) {
                        // Two sections in one slot (Goal, In flight) read as
                        // two paragraphs.
                        if !sections[slot].is_empty() {
                            sections[slot].push("");
                        }
                        current = Some((slot, level));
                        continue;
                    }
                    if current.is_some_and(|(_, open)| level <= open) {
                        current = None;
                    }
                }
            }
        }
        if let Some((slot, _)) = current {
            sections[slot].push(line);
        }
    }

    // Prose sections keep their markdown, minus filler lines ("- None").
    let prose = |lines: &[&str]| {
        let kept: Vec<&str> = lines
            .iter()
            .copied()
            .filter(|l| {
                let t = l.trim();
                t.is_empty() || !is_placeholder(list_marker(t).unwrap_or(t))
            })
            .collect();
        cap_text(tidy(&kept))
    };
    // List sections are items; a section written as prose yields its
    // paragraphs instead.
    let list = |lines: &[&str]| {
        let mut items = list_items(lines.iter().copied());
        if items.is_empty() {
            items = paragraphs(lines)
                .into_iter()
                .map(|text| ListItem { text, done: false })
                .collect();
        }
        live_items(items, MAX_LIST_ITEMS)
    };
    let left = LeftOff {
        worked_on: prose(&sections[0]),
        decisions: prose(&sections[1]),
        blockers: list(&sections[2]),
        current: prose(&sections[3]),
        next: list(&sections[4]),
        written_ms: source.stat.mtime_ms,
        path: source.rel.clone(),
        ..LeftOff::default()
    };
    let empty = left.worked_on.is_empty()
        && left.decisions.is_empty()
        && left.blockers.is_empty()
        && left.current.is_empty()
        && left.next.is_empty();
    if empty {
        notes.push(format!("{}: no handoff sections with content", source.rel));
        return None;
    }
    Some(left)
}

// ---------------------------------------------------------------------------
// Todos
// ---------------------------------------------------------------------------

/// A registry link target, made workspace-relative (the registry links
/// relative to `todo/`). Absolute paths, URLs, and targets that would climb
/// out of the workspace stay verbatim.
fn todo_path(target: &str) -> String {
    relative_path("todo", target)
}

/// `target` as written in a file under `base`, made workspace-relative.
/// Absolute paths, URLs, and targets that would climb out of the workspace
/// stay verbatim.
fn relative_path(base: &str, target: &str) -> String {
    let target = target.trim();
    let path = target.split(['#', '?']).next().unwrap_or("");
    if path.is_empty() {
        return String::new();
    }
    if path.contains("://") || path.starts_with(['/', '~', '\\']) {
        return cap_text(target.to_owned());
    }
    let mut parts: Vec<&str> = base.split('/').collect();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if parts.pop().is_none() {
                    return cap_text(target.to_owned());
                }
            }
            part => parts.push(part),
        }
    }
    cap_text(parts.join("/"))
}

/// Registry column indexes for item, priority, status, category, date,
/// author, file.
type TodoColumns = [Option<usize>; 7];

fn todo_columns(header: &[String]) -> Option<TodoColumns> {
    const NAMES: [&str; 7] = [
        "item", "priority", "status", "category", "date", "author", "file",
    ];
    let mut cols: TodoColumns = [None; 7];
    for (i, cell) in header.iter().enumerate() {
        let name = cell.trim_matches(['*', '`', ' ']).to_lowercase();
        if let Some(slot) = NAMES.iter().position(|n| *n == name) {
            cols[slot].get_or_insert(i);
        }
    }
    // Item plus Status or Priority: mycelium's Status/Priority key tables
    // ("Status | Meaning") have no Item column and must not match.
    (cols[0].is_some() && (cols[1].is_some() || cols[2].is_some())).then_some(cols)
}

/// `todo/TODO_REGISTRY.md`: the `Item | Priority | Status | …` table, found
/// by its header (mycelium's full template puts Status/Priority key tables
/// above it), and the `##` to-do sections agents write below it
/// (`## #50 — … ✅ DONE`, `## T-Name — …`). Rows are read anywhere after the
/// header, NOT only to the `<!-- Add new entries above this line -->`
/// marker or the next heading: mycelium 0.7.2's `upsert_table_row.py`
/// appends new rows at the end of the file, below whatever is there — so
/// a headerless row with the registry's width is a registry row, while
/// another table's rows are not. The legacy `todo/TODOLIST.md` had no
/// schema; its list items are todos. Also returns the names of the to-dos
/// kept as sections, which the registry table doesn't list (Tidy up).
fn parse_todos(text: &str, rel: &str, legacy: bool, notes: &mut Notes) -> (Vec<Todo>, Vec<String>) {
    let doc = Doc::new(text);
    doc.note_unclosed(rel, notes);
    if legacy {
        notes.push(format!(
            "{rel} is mycelium's legacy todo list; 0.7 keeps todos in todo/TODO_REGISTRY.md"
        ));
    }
    let row_at = |i: usize| doc.text(i).and_then(table_cells);
    let is_header = |i: usize| row_at(i + 1).is_some_and(|next| is_separator(&next));

    let header = (0..doc.lines.len()).find_map(|i| {
        let cells = row_at(i)?;
        if !is_header(i) {
            return None;
        }
        todo_columns(&cells).map(|cols| (i, cols, cells.len()))
    });
    let Some((header_at, cols, width)) = header else {
        if legacy {
            return (legacy_todos(&doc, rel, notes), Vec::new());
        }
        notes.push(format!("{rel}: no Item | Priority | Status table found"));
        return (Vec::new(), Vec::new());
    };

    #[derive(PartialEq)]
    enum Block {
        /// The registry table (its header, or a repeat of it).
        Registry,
        /// Another table: its rows are not to-dos.
        Foreign,
        /// Headerless rows outside any table: appended registry rows when
        /// they have its width.
        Loose,
        None,
    }
    let mut block = Block::Registry;
    // Every registry row's line (where an appended row ends a section),
    // but the cells of only the rows that can be shown.
    let mut row_lines: Vec<usize> = Vec::new();
    let mut rows: Vec<(usize, Vec<String>)> = Vec::new();
    let mut sections: Vec<usize> = Vec::new();
    // Every `#`/`##` heading after the header: where a section ends.
    let mut bounds: Vec<usize> = Vec::new();
    let mut keep_row = |i: usize, cells: Vec<String>| {
        row_lines.push(i);
        if rows.len() < MAX_ENTRIES {
            rows.push((i, cells));
        }
    };
    for i in header_at + 2..doc.lines.len() {
        let Some(line) = doc.text(i) else {
            continue;
        };
        if let Some((level, text)) = heading(line) {
            block = Block::None;
            if level <= 2 {
                bounds.push(i);
                if level == 2 && is_todo_section(text) {
                    sections.push(i);
                }
            }
            continue;
        }
        let Some(cells) = table_cells(line) else {
            // A blank line ends a table; a stray line of text inside the
            // registry doesn't.
            if block != Block::Registry || line.trim().is_empty() {
                block = Block::None;
            }
            continue;
        };
        if is_separator(&cells) {
            continue;
        }
        if is_header(i) {
            block = if todo_columns(&cells).is_some() {
                Block::Registry
            } else {
                Block::Foreign
            };
            continue;
        }
        match block {
            Block::Registry => keep_row(i, cells),
            Block::Foreign => {}
            Block::Loose | Block::None => {
                if cells.len() == width {
                    keep_row(i, cells);
                    block = Block::Loose;
                } else {
                    block = Block::Foreign;
                }
            }
        }
    }

    let mut todos = Vec::new();
    let total = row_lines.len() + sections.len();
    for (n, (i, cells)) in rows.iter().enumerate() {
        todos.extend(todo_row(
            cells,
            &cols,
            n + 1,
            span_of(&doc, rel, *i, *i + 1),
        ));
    }
    let mut off_index = Vec::new();
    for (n, &start) in sections.iter().enumerate() {
        if todos.len() == MAX_ENTRIES {
            break;
        }
        // A section ends at the next heading, or at a registry row appended
        // after it (both lists are in line order).
        let next = |lines: &[usize]| lines.get(lines.partition_point(|&l| l <= start)).copied();
        let end = [next(&bounds), next(&row_lines)]
            .into_iter()
            .flatten()
            .min()
            .unwrap_or(doc.lines.len());
        let todo = section_todo(&doc, rel, start, end, n + 1);
        off_index.push(if todo.id.is_empty() {
            format!("\"{}\"", todo.title)
        } else {
            todo.id.clone()
        });
        todos.push(todo);
    }
    if total > MAX_ENTRIES {
        notes.push(format!(
            "{rel}: showing the first {MAX_ENTRIES} of {total} todos"
        ));
    }
    // A repeated id keeps its key unique: `todo/#50`, `todo/#50~2`.
    let mut seen: HashMap<String, usize> = HashMap::new();
    for todo in &mut todos {
        let n = seen.entry(todo.key.clone()).or_insert(0);
        *n += 1;
        if *n > 1 {
            todo.key = format!("{}~{n}", todo.key);
        }
    }
    (todos, off_index)
}

/// A `##` heading below the registry that is a to-do, not structure (a key
/// table's or an archive's heading).
fn is_todo_section(text: &str) -> bool {
    let name = plain(text).to_lowercase();
    let name = name.trim_end_matches(':').trim();
    !(name.is_empty()
        || name.ends_with(" key")
        || matches!(
            name,
            "key" | "legend" | "registry" | "notes" | "archive" | "archived"
        ))
}

/// A to-do id opening `text`: `#50` or `T-GroupTiers`, and the rest after
/// its separator.
fn todo_id(text: &str) -> Option<(String, &str)> {
    let t = text.trim_start().trim_start_matches('*').trim_start();
    let id = if let Some(num) = t.strip_prefix('#') {
        let digits = num.bytes().take_while(u8::is_ascii_digit).count();
        (digits > 0).then(|| &t[..1 + digits])?
    } else {
        match id_at(t, 0) {
            Some(("todo", id)) => id,
            _ => return None,
        }
    };
    let rest = &t[id.len()..];
    if !(rest.is_empty()
        || rest.starts_with(|c: char| {
            c.is_whitespace() || matches!(c, '—' | '–' | '-' | ':' | '.' | '*')
        }))
    {
        return None;
    }
    let rest = rest.trim_start_matches(|c: char| {
        c.is_whitespace() || matches!(c, '—' | '–' | '-' | ':' | '.' | '*')
    });
    Some((id.to_owned(), rest))
}

/// A section heading that says the to-do is closed: ✅, or DONE / COMPLETE
/// as a word — but not `HALF DONE`, `NOT COMPLETE`.
fn heading_says_closed(text: &str) -> bool {
    if text.contains('✅') {
        return true;
    }
    let mut prev = "";
    for word in text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
    {
        if matches!(word, "DONE" | "COMPLETE" | "COMPLETED")
            && !matches!(
                prev.to_uppercase().as_str(),
                "HALF" | "NOT" | "PARTLY" | "PARTIALLY" | "NEARLY" | "ALMOST"
            )
        {
            return true;
        }
        prev = word;
    }
    false
}

/// A to-do item's lead: its bold opening when it has one (`**Build the
/// roster stage**: …`), else its first sentence; ≤ [`MAX_TITLE_CHARS`].
fn todo_title(item: &str) -> String {
    let t = item.trim();
    let lead = t
        .strip_prefix("**")
        .and_then(|rest| rest.find("**").map(|close| plain(&rest[..close])))
        .filter(|lead| !lead.trim_end_matches(':').trim().is_empty());
    let title = lead.map_or_else(
        || first_sentence(t),
        |l| l.trim_end_matches(':').trim().to_owned(),
    );
    cap_chars(&title, MAX_TITLE_CHARS)
}

/// The `##` section at `start..end` as a to-do: its heading is the item's
/// title, its fields (`**Status**:`, `**Priority**:`, `**Opened**:`, …) the
/// row's cells, its first paragraph the item's text.
fn section_todo(doc: &Doc, rel: &str, start: usize, end: usize, n: usize) -> Todo {
    let text = heading(doc.lines[start]).map_or("", |(_, text)| text);
    let (id, rest) = todo_id(text).unwrap_or((String::new(), text));
    let title = cap_chars(&plain(rest), MAX_TITLE_CHARS);
    let body = start + 1..end;
    let fields = Fields::parse(doc, std::slice::from_ref(&body), TODO_FIELDS);
    let cell = |names: &[&str]| fields.first(names).map(plain).unwrap_or_default();
    let status = cell(&["status"]).to_lowercase();
    let item = match first_prose(doc, &body, TODO_FIELDS) {
        Some(lead) => cap_text(format!("{}\n\n{lead}", plain(rest))),
        None => cap_text(plain(rest)),
    };
    let whole = start..end;
    let (refs, cites) = refs_and_cites(doc, std::slice::from_ref(&whole), &id);
    Todo {
        item,
        priority: cell(&["priority"]).to_lowercase(),
        closed: is_closed(&status) || heading_says_closed(text),
        status: cap_text(status),
        category: cap_text(cell(&["category"])),
        date: fields
            .date(&["date", "opened", "raised", "added"])
            .unwrap_or_default(),
        author: cap_text(cell(&["author", "owner"])),
        file: String::new(),
        key: if id.is_empty() {
            format!("todo/s{n}")
        } else {
            format!("todo/{id}")
        },
        id,
        title,
        source: "section",
        span: Some(span_of(doc, rel, start, end)),
        refs,
        cites,
    }
}

/// A registry row as a to-do. Its File cell is a link only when it is one
/// — a markdown link (relative to `todo/`) or a bare `item.md` (the
/// registry's own writeups); a free-text cell ("D-117; F-198; `notes/x`")
/// gives its ids as `refs` and its paths as `cites`, never a guessed link.
fn todo_row(cells: &[String], cols: &TodoColumns, row: usize, span: Span) -> Option<Todo> {
    let raw = |slot: usize| {
        cols[slot]
            .and_then(|i| cells.get(i))
            .map_or("", String::as_str)
    };
    let (_, item_link) = strip_links(raw(0));
    let item = cell_text(raw(0));
    if item.is_empty() {
        return None;
    }
    let (_, file_link) = strip_links(raw(6));
    let file_text = plain(&cell_text(raw(6)));
    let bare_md = (!file_text.contains(char::is_whitespace)
        && !file_text.contains('/')
        && file_text.len() > 3
        && file_text.ends_with(".md"))
    .then_some(file_text);
    let file = file_link
        .or(bare_md)
        .or(item_link)
        .map_or_else(String::new, |target| todo_path(&target));
    let id = todo_id(&plain(&item)).map(|(id, _)| id).unwrap_or_default();
    let mut refs = Vec::new();
    let mut cites = Vec::new();
    for cell in [raw(0), raw(6)] {
        scan_refs(cell, &id, &mut refs);
        scan_cites(cell, &mut cites);
    }
    let status = plain(&cell_text(raw(2))).to_lowercase();
    Some(Todo {
        title: todo_title(&item),
        item,
        priority: plain(&cell_text(raw(1))).to_lowercase(),
        closed: is_closed(&status),
        status,
        category: cell_text(raw(3)),
        date: cell_text(raw(4)),
        author: cell_text(raw(5)),
        file,
        key: if id.is_empty() {
            format!("todo/r{row}")
        } else {
            format!("todo/{id}")
        },
        id,
        source: "table",
        span: Some(span),
        refs,
        cites,
    })
}

fn legacy_todos(doc: &Doc, rel: &str, notes: &mut Notes) -> Vec<Todo> {
    let lines = (0..doc.lines.len()).filter_map(|i| doc.text(i));
    let items = list_items(lines);
    if items.len() > MAX_ENTRIES {
        notes.push(format!(
            "{rel}: showing the first {MAX_ENTRIES} of {} todos",
            items.len()
        ));
    }
    items
        .into_iter()
        .filter(|item| !is_placeholder(&item.text))
        .take(MAX_ENTRIES)
        .enumerate()
        .map(|(n, item)| {
            let (text, link) = strip_links(&item.text);
            let item_text = cap_text(text.trim().to_owned());
            let status = if item.done { "complete" } else { "open" };
            let mut refs = Vec::new();
            scan_refs(&item.text, "", &mut refs);
            Todo {
                title: todo_title(&item_text),
                item: item_text,
                status: status.to_owned(),
                closed: item.done,
                file: link.map_or_else(String::new, |target| todo_path(&target)),
                key: format!("todo/r{}", n + 1),
                source: "table",
                refs,
                ..Todo::default()
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Conventions and sessions
// ---------------------------------------------------------------------------

/// `.living/conventions.md`: each `##` section is a convention — `## C-12 —
/// title` (id `C-12`), or a plain `## title` without one.
fn parse_conventions(text: &str, rel: &str, notes: &mut Notes) -> Vec<Convention> {
    let (doc, _) = Doc::with_frontmatter(text);
    doc.note_unclosed(rel, notes);
    let mut heads: Vec<(usize, &str)> = Vec::new();
    let mut bounds: Vec<usize> = Vec::new();
    for i in 0..doc.lines.len() {
        if let Some((level, text)) = doc.text(i).and_then(heading) {
            if level <= 2 {
                bounds.push(i);
            }
            if level == 2 {
                heads.push((i, text));
            }
        }
    }
    let mut out = Vec::new();
    let mut seen: HashMap<String, usize> = HashMap::new();
    for (n, &(start, text)) in heads.iter().enumerate().take(MAX_CONVENTIONS) {
        let end = bounds
            .iter()
            .copied()
            .find(|&b| b > start)
            .unwrap_or(doc.lines.len());
        let (id, rest) = explicit_id(unbold(text), b'C').unwrap_or((String::new(), text));
        // The heading's date moves to `date`, as for findings and decisions.
        let (rest, date) = title_and_date(rest);
        let title = match plain(&rest) {
            t if t.is_empty() => plain(text),
            t => t,
        };
        let body = start + 1..end;
        let fields = Fields::parse(&doc, std::slice::from_ref(&body), CONVENTION_FIELDS);
        let whole = start..end;
        let (refs, cites) = refs_and_cites(&doc, std::slice::from_ref(&whole), &id);
        let base = if id.is_empty() {
            format!("conventions/s{}", n + 1)
        } else {
            format!("conventions/{id}")
        };
        let k = seen.entry(base.clone()).or_insert(0);
        *k += 1;
        out.push(Convention {
            key: if *k == 1 { base } else { format!("{base}~{k}") },
            id,
            title: cap_text(title),
            status: fields.stated(&["status"]),
            date: date.map(str::to_owned).unwrap_or_default(),
            span: span_of(&doc, rel, start, end),
            refs,
            cites,
        });
    }
    if heads.len() > MAX_CONVENTIONS {
        notes.push(format!(
            "{rel}: showing the first {MAX_CONVENTIONS} of {} conventions",
            heads.len()
        ));
    }
    out
}

/// `.living/generated-conventions/<dir>/convention.md` (mycelium's
/// crystallize output): its frontmatter `id`, `title` and `status`.
fn parse_generated_convention(text: &str, rel: &str, dir: &str) -> Option<Convention> {
    let (doc, front) = Doc::with_frontmatter(text);
    let meta = |key: &str| {
        front
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.trim().to_owned())
            .filter(|v| !is_placeholder(v))
    };
    let heading_title = (0..doc.lines.len())
        .find_map(|i| doc.text(i).and_then(heading).filter(|(l, _)| *l == 1))
        .map(|(_, t)| plain(t));
    let title = meta("title")
        .or(heading_title)
        .unwrap_or_else(|| dir.to_owned());
    let all = 0..doc.lines.len();
    let (refs, cites) = refs_and_cites(&doc, std::slice::from_ref(&all), "");
    Some(Convention {
        key: format!("generated-conventions/{dir}"),
        id: meta("id").map(cap_text).unwrap_or_default(),
        title: cap_text(title),
        status: meta("status").map(cap_text).unwrap_or_default(),
        date: meta("created")
            .filter(|d| d.as_bytes().get(..10).is_some_and(is_iso_date))
            .map(|d| d[..10].to_owned())
            .unwrap_or_default(),
        span: span_of(&doc, rel, 0, doc.lines.len()),
        refs,
        cites,
    })
}

/// Session log columns: date, session id, branch, duration, files changed,
/// summary, key outputs, status, log.
type SessionColumns = [Option<usize>; 9];

fn session_columns(header: &[String]) -> Option<SessionColumns> {
    let mut cols: SessionColumns = [None; 9];
    for (i, cell) in header.iter().enumerate() {
        let name = plain(cell).to_lowercase();
        let slot = match name.as_str() {
            "date" => 0,
            "session id" | "session" | "id" => 1,
            "branch" => 2,
            "duration" => 3,
            "files changed" | "files" => 4,
            "summary" => 5,
            "key outputs" | "outputs" => 6,
            "status" => 7,
            "log" => 8,
            _ => continue,
        };
        cols[slot].get_or_insert(i);
    }
    (cols[0].is_some() && cols[1].is_some()).then_some(cols)
}

/// `.living/log/LOG_REGISTRY.md`'s rows, newest first.
fn parse_sessions(text: &str, rel: &str, notes: &mut Notes) -> Vec<Session> {
    let doc = Doc::new(text);
    doc.note_unclosed(rel, notes);
    let row_at = |i: usize| doc.text(i).and_then(table_cells);
    let is_header = |i: usize| row_at(i + 1).is_some_and(|next| is_separator(&next));
    let Some((header_at, cols, width)) = (0..doc.lines.len()).find_map(|i| {
        let cells = row_at(i)?;
        if !is_header(i) {
            return None;
        }
        session_columns(&cells).map(|cols| (i, cols, cells.len()))
    }) else {
        return Vec::new();
    };
    // The registry appends, so the last rows are the newest: only those
    // are kept while reading.
    let mut rows: VecDeque<(usize, Session)> = VecDeque::new();
    let mut total = 0usize;
    for i in header_at + 2..doc.lines.len() {
        let Some(cells) = row_at(i) else {
            continue;
        };
        if is_separator(&cells) || is_header(i) || cells.len() != width {
            continue;
        }
        let cell = |slot: usize| {
            cols[slot]
                .and_then(|i| cells.get(i))
                .map_or_else(String::new, |c| cell_text(c))
        };
        let id = cell(1);
        if id.is_empty() {
            continue;
        }
        let log = cols[8]
            .and_then(|i| cells.get(i))
            .and_then(|c| strip_links(c).1)
            .map_or_else(String::new, |target| relative_path(".living/log", &target));
        total += 1;
        if rows.len() == MAX_SESSIONS {
            rows.pop_front();
        }
        rows.push_back((
            i,
            Session {
                id,
                date: cell(0),
                branch: cell(2),
                duration: cell(3),
                files: cell(4),
                summary: cell(5),
                outputs: cell(6),
                status: cell(7),
                log,
            },
        ));
    }
    if total > rows.len() {
        notes.push(format!(
            "{rel}: showing the newest {MAX_SESSIONS} of {total} sessions"
        ));
    }
    let mut rows: Vec<(usize, Session)> = rows.into();
    rows.sort_by(|a, b| b.1.date.cmp(&a.1.date).then(b.0.cmp(&a.0)));
    rows.into_iter().map(|(_, s)| s).collect()
}

// ---------------------------------------------------------------------------
// The final pass: what only the whole snapshot knows
// ---------------------------------------------------------------------------

fn finish(k: &mut Knowledge, handoff: HandoffRead, todo_sections: &[String]) {
    let HandoffRead { left, asks, stub } = handoff;
    k.left_off = left;

    // A `T-Name` is a to-do only where one has that id ("T-cell" is prose).
    let todo_ids: HashSet<String> = k
        .todos
        .iter()
        .filter(|t| t.id.starts_with("T-"))
        .map(|t| t.id.clone())
        .collect();
    let keep = |refs: &mut Vec<Ref>| refs.retain(|r| r.kind != "todo" || todo_ids.contains(&r.id));
    for f in k.topics.iter_mut().flat_map(|t| t.findings.iter_mut()) {
        keep(&mut f.refs);
    }
    k.decisions.iter_mut().for_each(|d| keep(&mut d.refs));
    k.learnings.iter_mut().for_each(|l| keep(&mut l.refs));
    k.todos.iter_mut().for_each(|t| keep(&mut t.refs));
    k.conventions.iter_mut().for_each(|c| keep(&mut c.refs));

    apply_amends(k);
    for f in k.topics.iter_mut().flat_map(|t| t.findings.iter_mut()) {
        if f.state.is_none() && f.pending.resolved {
            f.state = Some(State {
                kind: "resolved",
                by: None,
            });
        }
    }
    k.asks = collect_asks(k, asks);
    k.tidy = tidy_rows(k, todo_sections, stub);

    let open_todos = k.todos.iter().filter(|t| !t.closed).count();
    k.counts = Counts {
        findings: count(k.topics.iter().map(|t| t.findings.len()).sum()),
        decisions: count(k.decisions.len()),
        learnings: count(k.learnings.len()),
        open: count(open_todos + k.questions.len()),
        todos: count(open_todos),
        questions: count(k.questions.len()),
        conventions: count(k.conventions.len()),
        sessions: count(k.sessions.len()),
    };
    k.id_shapes = id_shapes();
    k.labels = labels();
}

/// What an amend makes of its target, and how strong that is: an entry
/// another retracts reads retracted even if a third only corrected it.
fn inverse(kind: &str) -> (&'static str, u8) {
    match kind {
        "retracts" => ("retracted", 3),
        "supersedes" => ("superseded", 2),
        _ => ("corrected", 1),
    }
}

/// Each amend's inverse on its target: `F-178 corrects F-171` makes F-171
/// `corrected` by F-178. A target id naming several findings resolves to
/// the one in the amending finding's topic, else to none; a marker the
/// target wrote itself is never overridden.
fn apply_amends(k: &mut Knowledge) {
    #[derive(Clone, Copy)]
    enum Target {
        Finding(usize, usize),
        Decision(usize),
    }
    let mut findings: HashMap<&str, Vec<(usize, usize)>> = HashMap::new();
    for (t, topic) in k.topics.iter().enumerate() {
        for (f, finding) in topic.findings.iter().enumerate() {
            findings.entry(&finding.id).or_default().push((t, f));
        }
    }
    let mut decisions: HashMap<&str, Vec<usize>> = HashMap::new();
    for (d, decision) in k.decisions.iter().enumerate() {
        if !decision.id.is_empty() {
            decisions.entry(&decision.id).or_default().push(d);
        }
    }
    let resolve = |id: &str, topic: Option<usize>| -> Option<Target> {
        if id.starts_with("F-") {
            let all = findings.get(id)?;
            if let [(t, f)] = all.as_slice() {
                return Some(Target::Finding(*t, *f));
            }
            let mut same = all.iter().filter(|(t, _)| Some(*t) == topic);
            match (same.next(), same.next()) {
                (Some(&(t, f)), None) => Some(Target::Finding(t, f)),
                _ => None,
            }
        } else if id.starts_with("D-") {
            match decisions.get(id)?.as_slice() {
                [d] => Some(Target::Decision(*d)),
                _ => None,
            }
        } else {
            None
        }
    };
    let mut hits: Vec<(Target, &'static str, u8, String)> = Vec::new();
    for (t, topic) in k.topics.iter().enumerate() {
        for finding in &topic.findings {
            for amend in &finding.amends {
                if let Some(target) = resolve(&amend.id, Some(t)) {
                    let (kind, rank) = inverse(amend.kind);
                    hits.push((target, kind, rank, finding.id.clone()));
                }
            }
        }
    }
    for decision in &k.decisions {
        for amend in &decision.amends {
            if let Some(target) = resolve(&amend.id, None) {
                let (kind, rank) = inverse(amend.kind);
                hits.push((target, kind, rank, decision.id.clone()));
            }
        }
    }
    for (target, kind, rank, by) in hits {
        let (state, own) = match target {
            Target::Finding(t, f) => {
                let f = &mut k.topics[t].findings[f];
                (&mut f.state, f.pending.own_state)
            }
            Target::Decision(d) => {
                let d = &mut k.decisions[d];
                (&mut d.state, d.pending.own_state)
            }
        };
        if own {
            continue;
        }
        let weaker = state.as_ref().is_none_or(|s| inverse_rank(s.kind) < rank);
        if weaker {
            *state = Some(State {
                kind,
                by: (!by.is_empty()).then_some(by),
            });
        }
    }
}

fn inverse_rank(kind: &str) -> u8 {
    match kind {
        "retracted" => 3,
        "superseded" => 2,
        "corrected" => 1,
        _ => 0,
    }
}

/// Whether the text says an entry is settled — superseded, retracted,
/// corrected or resolved — so what it once put to the user no longer waits.
fn settled(state: Option<&State>) -> bool {
    state.is_some_and(|s| {
        matches!(
            s.kind,
            "superseded" | "retracted" | "corrected" | "resolved"
        )
    })
}

/// "Waiting on you": the chosen handoff's asks (any date), then those of
/// findings and decisions the text doesn't call settled, each dated by the
/// part that wrote it and kept when within [`ASK_WINDOW_DAYS`] of the newest
/// dated entry — newest first, deduplicated, at most [`MAX_ASKS`].
fn collect_asks(k: &Knowledge, handoff: Vec<AskItem>) -> Vec<AskItem> {
    let findings = k.topics.iter().flat_map(|t| &t.findings);
    let newest = findings
        .clone()
        .filter_map(|f| f.pending.day)
        .chain(k.decisions.iter().filter_map(|d| d.pending.day))
        .max();
    let mut out = handoff;
    if let Some(newest) = newest {
        let recent = |day: Option<i64>| day.filter(|&d| d >= newest - ASK_WINDOW_DAYS);
        let date = |day: i64| date_of_ms(u64::try_from(day).unwrap_or(0) * 86_400_000);
        for f in findings.filter(|f| !settled(f.state.as_ref())) {
            for (text, span, day) in &f.pending.asks {
                let Some(day) = recent(*day) else {
                    continue;
                };
                out.push(AskItem {
                    text: text.clone(),
                    date: date(day),
                    source: AskSource {
                        kind: "finding",
                        id: f.id.clone(),
                        key: f.key.clone(),
                    },
                    span: span.clone(),
                });
            }
        }
        for d in k.decisions.iter().filter(|d| !settled(d.state.as_ref())) {
            for (text, span, day) in &d.pending.asks {
                let Some(day) = recent(*day) else {
                    continue;
                };
                out.push(AskItem {
                    text: text.clone(),
                    date: date(day),
                    source: AskSource {
                        kind: "decision",
                        id: d.id.clone(),
                        key: d.fp.clone(),
                    },
                    span: span.clone(),
                });
            }
        }
    }
    // Stable: on a tie the handoff's lead.
    out.sort_by(|a, b| b.date.cmp(&a.date));
    let mut seen = HashSet::new();
    out.retain(|a| seen.insert(a.text.to_lowercase()));
    out.truncate(MAX_ASKS);
    out
}

/// `a, b, c and 3 more` over at most `max` names.
fn name_list(names: &[String], max: usize) -> String {
    let shown: Vec<&str> = names.iter().take(max).map(String::as_str).collect();
    let mut out = shown.join(", ");
    if names.len() > max {
        out.push_str(&format!(" and {} more", names.len() - max));
    }
    out
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// The Tidy up rows: factual inconsistencies in the knowledge itself,
/// each with the request an agent would need. Never a status judgment.
fn tidy_rows(k: &Knowledge, todo_sections: &[String], stub: Option<Tidy>) -> Vec<Tidy> {
    let mut rows = Vec::new();

    // Finding ids naming more than one finding.
    let mut order: Vec<String> = Vec::new();
    let mut places: HashMap<String, Vec<String>> = HashMap::new();
    for topic in &k.topics {
        for f in &topic.findings {
            let at = places.entry(f.id.clone()).or_insert_with(|| {
                order.push(f.id.clone());
                Vec::new()
            });
            at.push(format!("{}:{}", topic.path, f.line));
        }
    }
    let mut dup: Vec<&String> = order.iter().filter(|id| places[*id].len() > 1).collect();
    dup.sort_by_key(|id| id[2..].parse::<u64>().unwrap_or(u64::MAX));
    if !dup.is_empty() {
        let ids: Vec<String> = dup.iter().map(|id| (*id).clone()).collect();
        let details: Vec<String> = dup
            .iter()
            .map(|id| format!("{id} ({})", places[*id].join(", ")))
            .collect();
        rows.push(Tidy {
            kind: "duplicate-id",
            text: cap_text(format!(
                "{} more than one finding: {}.",
                plural(ids.len(), "finding id names", "finding ids each name"),
                name_list(&ids, 12)
            )),
            refs: ids
                .iter()
                .take(MAX_TIDY_REFS)
                .map(|id| Ref {
                    kind: "finding",
                    id: id.clone(),
                })
                .collect(),
            ask: cap_bytes(
                format!(
                    "In .living/findings/, these finding ids each name more than one finding: {}. Keep each id on its earliest finding, give every later one the next unused F- number, and update the references to each renumbered finding in .living/ and todo/ — check which finding a reference means before changing it.",
                    details.join("; ")
                ),
                MAX_ASK_BYTES,
            ),
        });
    }

    // Explicit decision ids naming more than one decision.
    let mut d_order: Vec<&str> = Vec::new();
    let mut d_lines: HashMap<&str, Vec<u32>> = HashMap::new();
    let mut by_line: Vec<&Decision> = k.decisions.iter().collect();
    by_line.sort_by_key(|d| d.line);
    for d in by_line.iter().filter(|d| !d.id.is_empty()) {
        let lines = d_lines.entry(&d.id).or_insert_with(|| {
            d_order.push(&d.id);
            Vec::new()
        });
        lines.push(d.line);
    }
    let d_dup: Vec<&str> = d_order
        .into_iter()
        .filter(|id| d_lines[id].len() > 1)
        .collect();
    if !d_dup.is_empty() {
        let ids: Vec<String> = d_dup.iter().map(|id| (*id).to_owned()).collect();
        let details: Vec<String> = d_dup
            .iter()
            .map(|id| {
                let lines: Vec<String> = d_lines[id].iter().map(u32::to_string).collect();
                format!("{id} (lines {})", lines.join(", "))
            })
            .collect();
        rows.push(Tidy {
            kind: "duplicate-id",
            text: cap_text(format!(
                "{} more than one decision in .living/decisions.md: {}.",
                plural(ids.len(), "decision id heads", "decision ids each head"),
                name_list(&ids, 12)
            )),
            refs: ids
                .iter()
                .take(MAX_TIDY_REFS)
                .map(|id| Ref {
                    kind: "decision",
                    id: id.clone(),
                })
                .collect(),
            ask: cap_bytes(
                format!(
                    "In .living/decisions.md, these decision ids each head more than one entry: {}. Keep each id on its earliest entry, give every later one the next unused D- number, and update the references to each renumbered decision in .living/ and todo/ — check which entry a reference means before changing it.",
                    details.join("; ")
                ),
                MAX_ASK_BYTES,
            ),
        });
    }

    // To-dos kept as sections, outside the registry table.
    if !todo_sections.is_empty() {
        let n = todo_sections.len();
        rows.push(Tidy {
            kind: "off-index",
            text: cap_text(format!(
                "{} kept as ## sections below the registry table in todo/TODO_REGISTRY.md, not as rows in it: {}.",
                plural(n, "to-do is", "to-dos are"),
                name_list(todo_sections, 12)
            )),
            refs: todo_sections
                .iter()
                .filter(|name| name.starts_with("T-"))
                .take(MAX_TIDY_REFS)
                .map(|id| Ref {
                    kind: "todo",
                    id: id.clone(),
                })
                .collect(),
            ask: cap_bytes(
                format!(
                    "todo/TODO_REGISTRY.md keeps these to-dos as ## sections below its registry table instead of as rows in it: {}. Add a registry row for each (Item, Priority, Status, Category, Date, Author, File) that summarizes it and names its section, so mycelium's registry lists it; keep each section as its writeup.",
                    todo_sections.join(", ")
                ),
                MAX_ASK_BYTES,
            ),
        });
    }

    // Decisions and learnings written as `##` entries.
    let mut off: Vec<(String, String)> = Vec::new();
    for d in k.decisions.iter().filter(|d| d.pending.off_index) {
        let name = if d.id.is_empty() { &d.title } else { &d.id };
        off.push((name.clone(), format!(".living/decisions.md:{}", d.line)));
    }
    for l in k.learnings.iter().filter(|l| l.pending.off_index) {
        let name = if l.id.is_empty() { &l.title } else { &l.id };
        off.push((name.clone(), format!(".living/learnings.md:{}", l.line)));
    }
    off.sort_by(|a, b| a.1.cmp(&b.1));
    if !off.is_empty() {
        let names: Vec<String> = off.iter().map(|(name, _)| name.clone()).collect();
        let details: Vec<String> = off
            .iter()
            .map(|(name, at)| format!("{name} ({at})"))
            .collect();
        let refs = k
            .decisions
            .iter()
            .filter(|d| d.pending.off_index && !d.id.is_empty())
            .map(|d| Ref {
                kind: "decision",
                id: d.id.clone(),
            })
            .chain(
                k.learnings
                    .iter()
                    .filter(|l| l.pending.off_index && !l.id.is_empty())
                    .map(|l| Ref {
                        kind: "learning",
                        id: l.id.clone(),
                    }),
            )
            .take(MAX_TIDY_REFS)
            .collect();
        rows.push(Tidy {
            kind: "off-index",
            text: cap_text(format!(
                "{} written as ## headings, which mycelium's index (### entries) doesn't see: {}.",
                plural(off.len(), "entry is", "entries are"),
                name_list(&names, 12)
            )),
            refs,
            ask: cap_bytes(
                format!(
                    "These entries are written as ## headings, which mycelium's index and tools (### entries only) don't see: {}. Change each to a ### heading, keeping its id, title and date as written.",
                    details.join("; ")
                ),
                MAX_ASK_BYTES,
            ),
        });
    }

    rows.extend(stub);

    // Open to-dos with the same title.
    let mut t_order: Vec<String> = Vec::new();
    let mut same: HashMap<String, Vec<&Todo>> = HashMap::new();
    for todo in k.todos.iter().filter(|t| !t.closed && !t.title.is_empty()) {
        let norm = collapse_ws(&todo.title.to_lowercase());
        same.entry(norm.clone())
            .or_insert_with(|| {
                t_order.push(norm);
                Vec::new()
            })
            .push(todo);
    }
    let twins: Vec<&Vec<&Todo>> = t_order
        .iter()
        .map(|t| &same[t])
        .filter(|list| list.len() > 1)
        .collect();
    if !twins.is_empty() {
        let names: Vec<String> = twins
            .iter()
            .map(|list| format!("\"{}\" (×{})", list[0].title, list.len()))
            .collect();
        let details: Vec<String> = twins
            .iter()
            .map(|list| {
                let lines: Vec<String> = list
                    .iter()
                    .map(|t| {
                        t.span
                            .as_ref()
                            .map_or_else(|| t.key.clone(), |s| format!("line {}", s.line))
                    })
                    .collect();
                format!("\"{}\" ({})", list[0].title, lines.join(", "))
            })
            .collect();
        rows.push(Tidy {
            kind: "duplicate-todo",
            text: cap_text(format!(
                "{} the same title: {}.",
                plural(twins.len(), "pair of open to-dos has", "sets of open to-dos have"),
                name_list(&names, 12)
            )),
            refs: Vec::new(),
            ask: cap_bytes(
                format!(
                    "In todo/TODO_REGISTRY.md, these open to-dos have the same title: {}. Check whether each set is the same work; if so, merge them into one entry that keeps every detail and reference, otherwise make their titles say how they differ.",
                    details.join("; ")
                ),
                MAX_ASK_BYTES,
            ),
        });
    }
    rows
}

/// The id shapes this plugin answers for, as JavaScript regex sources. A
/// to-do name may open with digits (`T-07Linkage`) but needs a letter.
fn id_shapes() -> Vec<IdShape> {
    vec![
        IdShape {
            kind: "finding",
            pattern: r"F-\d{1,4}",
        },
        IdShape {
            kind: "decision",
            pattern: r"D-\d{1,4}",
        },
        IdShape {
            kind: "convention",
            pattern: r"C-\d{1,3}",
        },
        IdShape {
            kind: "learning",
            pattern: r"L-\d{1,4}",
        },
        IdShape {
            kind: "todo",
            pattern: r"T-\d*[A-Za-z][A-Za-z0-9]*",
        },
    ]
}

/// The plugin's words for the Knowledge view.
fn labels() -> Labels {
    Labels {
        source: "Mycelium",
        sections: SectionLabels {
            overview: "Overview",
            left_off: "Where we left off",
            asks: "Waiting on you",
            changed: "What changed",
            open_work: "Open work",
            findings: "Findings",
            decisions: "Decisions",
            learnings: "Watch out for",
            conventions: "Conventions",
            todos: "To do",
            sessions: "Sessions",
            tidy: "Tidy up",
        },
        kinds: KindLabels {
            finding: "finding",
            decision: "decision",
            learning: "learning",
            convention: "convention",
            todo: "to-do",
            session: "session",
        },
        status_words: vec![
            StatusWord {
                word: "preliminary",
                rank: 1,
                tone: "neutral",
            },
            StatusWord {
                word: "supported",
                rank: 2,
                tone: "good",
            },
            StatusWord {
                word: "robust",
                rank: 3,
                tone: "good",
            },
            StatusWord {
                word: "contradicted",
                rank: 0,
                tone: "bad",
            },
        ],
        status_note: "The status is what the agent wrote when it recorded the finding. \
                      Mycelium's template describes a ladder — preliminary (one evidence row), \
                      supported (two or more that agree), robust (three or more across datasets \
                      or projects), contradicted (any row against it) — but the agent applies \
                      it; Chimaera shows what was written and never rates a finding.",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::StdFs;
    use chimaera_plugin_api::serde_json;
    use std::fs::File;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, SystemTime};

    /// The reader over a real tree, through `std::fs` under the host's rules.
    pub(super) fn read(root: &Path) -> Knowledge {
        let fs = StdFs::new(root);
        super::read(&fs, plan(&fs))
    }

    fn stamp(root: &Path) -> Stamp {
        plan(&StdFs::new(root)).stamp()
    }

    /// A throwaway workspace root, removed on drop.
    pub(super) struct Fixture(PathBuf);

    impl Fixture {
        pub(super) fn new(label: &str) -> Self {
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let dir = std::env::temp_dir().join(format!(
                "chimaera-mycelium-{label}-{}-{}",
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        pub(super) fn root(&self) -> &Path {
            &self.0
        }

        pub(super) fn write(&self, rel: &str, body: &str) -> &Self {
            let path = self.0.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, body).unwrap();
            self
        }

        /// A copy of a checked-in tree under `fixtures/`.
        pub(super) fn copy_of(label: &str, tree: &str) -> Self {
            fn copy(from: &Path, to: &Path) {
                for entry in std::fs::read_dir(from).unwrap() {
                    let entry = entry.unwrap();
                    let dest = to.join(entry.file_name());
                    if entry.file_type().unwrap().is_dir() {
                        std::fs::create_dir_all(&dest).unwrap();
                        copy(&entry.path(), &dest);
                    } else {
                        std::fs::copy(entry.path(), &dest).unwrap();
                    }
                }
            }
            let fx = Self::new(label);
            let from = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("fixtures")
                .join(tree);
            copy(&from, fx.root());
            fx
        }

        /// The file's mtime, at `secs` since the epoch.
        pub(super) fn set_mtime_at(&self, rel: &str, secs: u64) {
            File::options()
                .write(true)
                .open(self.0.join(rel))
                .unwrap()
                .set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(secs))
                .unwrap();
        }

        fn set_mtime(&self, rel: &str, secs_ago: u64) {
            let when = SystemTime::now() - Duration::from_secs(secs_ago);
            File::options()
                .write(true)
                .open(self.0.join(rel))
                .unwrap()
                .set_modified(when)
                .unwrap();
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    const BATCH_EFFECTS: &str = r#"---
topic: batch-effects
description: How sequencing batch shifts expression estimates across runs.
created: 2026-08-01
last_updated: 2026-09-10
status: active
---

# Batch Effects

## F-003: Batch 2 inflates the mitochondrial fraction
**Status:** supported
**Claim:** Libraries sequenced in batch 2 show a ~4% higher mitochondrial read fraction than batch 1 after QC.
**Implications:** Regress out batch before comparing mito-based QC across runs.
**Tags:** qc, batch, mitochondria

### Evidence Ledger
| Date | Run/Session | Dataset | Project | Result | Direction |
|------|-------------|---------|---------|--------|-----------|
| 2026-08-14 | run-17 | pbmc-10k | atlas | +4.1% mito in batch 2 | supports |
| 2026-09-10 | [run-22](../log/2026-09-10-001-qc.md) | pbmc-20k | atlas | +3.8% mito in batch 2 | supports |

### Open Questions
- Does the shift persist after ambient RNA removal?
"#;

    const QC_THRESHOLDS: &str = r#"---
topic: qc-thresholds
description: "Per-sample QC cutoffs that hold across tissues."
last_updated: 2026-08-20
---

# QC thresholds

## F-005: MAD-based cutoffs beat fixed thresholds
**Status:** supported
**Claim:** Three-MAD cutoffs keep more good cells than fixed 500-gene floors.
**Implications:** Use MAD cutoffs per sample.
**Tags:** [qc, thresholds]

### Evidence Ledger
| Date | Run/Session | Dataset | Project | Result | Direction |
|---|---|---|---|---|---|
| 2026-08-18 | run-19 | kidney | atlas | 3 MAD retained 12% more cells | supports |
| 2026-08-20 | run-20 | lung | atlas | same pattern | supports |

---

## F-004: Doublet rate scales with loading density
**Status:** preliminary
**Claim:** Doublets rise roughly linearly with cells loaded per lane.
**Tags:** #doublets #qc

### Evidence Ledger
| Date | Run/Session | Dataset | Project | Result | Direction |
|---|---|---|---|---|---|
| 2026-08-19 | run-19 | kidney | atlas | 8% doublets at 20k loading | supports |

### Open Questions
- Is the scaling linear above 20k cells?
- {Gap in evidence that needs more data}
"#;

    const EXHAUSTION: &str = r#"---
topic: exhaustion
description: T-cell exhaustion signatures in tumour infiltrates.
---

# Exhaustion

## F-001: TOX marks terminal exhaustion
**Status:** robust
**Claim:** A TOX+ PD1-high cluster appears in every tumour cohort.
**Tags:** tcell, exhaustion

### Evidence Ledger
| Date | Run/Session | Dataset | Project | Result | Direction |
|---|---|---|---|---|---|
| 2026-07-02 | run-3 | melanoma | tumour | TOX+ PD1hi cluster | supports |
| 2026-07-20 | run-8 | nsclc | tumour | same cluster | supports |
| 2026-08-05 | run-12 | crc | colon | only in MSI-high | refines |

## F-002: TCF7 predicts response
**Status:** contradicted
**Claim:** TCF7+ CD8 fraction predicts checkpoint response.

### Evidence Ledger
| Date | Run/Session | Dataset | Project | Result | Direction |
|---|---|---|---|---|---|
| 2026-07-05 | run-4 | melanoma | tumour | TCF7+ enriched in responders | supports |
| 2026-08-30 | run-15 | nsclc | tumour | no association | contradicts |

### Open Questions
- Is the melanoma association a cohort artefact?
"#;

    const DECISIONS: &str = r#"# Decision Log

Append-only log of non-obvious decisions and their rationale.

**Entry template:** copy from `skills/core/templates/decision-log-entry.md` (includes Context, Decision, Alternatives considered, Rationale, Consequences, Tags fields).

### [2026-08-02] Use scran size factors over CPM

**Context**: Library sizes vary ten-fold across the two sequencing batches.

**Decision**: Normalise with scran pooled size factors.

**Alternatives considered**:
- CPM — ignores composition bias
- SCTransform — too slow on 200k cells

**Rationale**: scran corrects composition bias and scales to the atlas.

**Consequences**: Normalisation now needs a quick clustering pass first.

**Tags**: [normalisation, scran]

### [2026-09-12] Drop sample S14 from the atlas

**Context**: S14 failed QC in both batches.

**Decision**: Exclude S14 from all downstream analyses.

**Alternatives considered**:
- Keep S14 with a batch covariate — it still dominated PC1

**Rationale**: 38% mitochondrial reads; no rescue possible.

**Consequences**: The cohort is n=23.

**Tags**: qc, exclusion

### [2026-08-20] Pin scanpy to 1.10

**Context**: 1.11 changed the default HVG flavour.

**Decision**: Pin scanpy==1.10.3 in the environment.

**Alternatives considered**: none

**Rationale**: Results must stay comparable with the July runs.

**Consequences**: Revisit before publication.

**Tags**: #environment #scanpy
"#;

    const LEARNINGS: &str = r#"# Learnings

Append-only log of gotchas, surprises, and insights.

### [2026-08-02] Scanpy drops genes with zero counts

**Category**: gotcha

**What happened**: `sc.pp.filter_genes` silently removed 1,204 genes before HVG selection.

**Why it matters**: Marker panels lose genes without any warning.

**Resolution**: Filter after subsetting the marker panel.

**Tags**: [scanpy, filtering]

**mitigation_type**: ambient-awareness

<!-- mitigation_type guidance:
  structural       — A test, assertion, type constraint, frozenset, or schema
                     validation has SHIPPED in the codebase.
-->

**structural_mitigation_candidate**: assert the panel survives filtering in test_markers.py

source: atlas

### [2026-08-04] Slurm array OOM at 64G

**Category**: failure

**What happened**: The doublet step ran out of memory on the 200k-cell object.
It died building the kNN graph.

**Why it matters**: Re-runs cost a day of queue time.

**Resolution**: Request 128G for arrays above 150k cells.

**Tags**: slurm, memory

### [2026-08-11] Empty droplets in lane 3

**Category**: edge-case

**What happened**: Lane 3 had 40% empty droplets that passed the UMI floor.

**Why it matters**: They cluster together and look like a novel population.

**Resolution**: Run EmptyDrops per lane.

**Tags**: [droplets]

### [2026-08-15] Ambient RNA dominates low-count cells

**Category**: insight

**What happened**: Below 800 UMIs the profile matches the soup.

**Why it matters**: Low-count clusters are mostly ambient.

**Resolution**: SoupX before clustering.

**Tags**: ambient

### [2026-08-19] Cache the kNN graph

**Category**: tip

**What happened**: Rebuilding the graph took 40 minutes per run.

**Why it matters**: Parameter sweeps were dominated by it.

**Resolution**: Persist `obsp` between sweeps.

**Tags**: performance
"#;

    const TODO_REGISTRY: &str = r#"# TODO Registry

All future work items for this project are tracked here.

## Status Key

| Status | Meaning |
|--------|---------|
| `open` | Not yet started |
| `blocked` | Waiting on something external |

## Registry

| Item | Priority | Status | Category | Date | Author | File |
|------|----------|--------|----------|------|--------|------|
| Compare with public datasets | idea | open | validation | 2026-08-01 | Martin | [compare-public-data.md](compare-public-data.md) |
| Re-run QC with MAD cutoffs | high | in-progress | analysis | 2026-08-21 | Martin | [rerun-qc.md](rerun-qc.md) |
| Get batch 3 FASTQs | critical | `blocked` | data | 2026-09-02 | Martin | — |

<!-- Add new entries above this line -->
| Write methods section | medium | complete | writing | 2026-09-15 | Martin | [methods.md](methods.md) |
"#;

    const LAST_SESSION: &str = r#"<!-- BEGIN MYCELIUM LIFECYCLE STATUS -->
Lifecycle status: accepted by Stop at 2026-09-15T18:02:11Z.
<!-- END MYCELIUM LIFECYCLE STATUS -->

SESSION RESUME — Last session (2026-09-15 17:40):

## What was worked on
- Re-ran QC on batches 1-2 with MAD cutoffs
- Drafted the methods section

## Key decisions made
- Dropped S14 (see .living/decisions.md for full context)

## Blockers & surprises
- Batch 3 FASTQs still not delivered
- None

## Current state
- Branch: `qc-mad` | Tests: 42 passing

## Next steps
1. Request batch 3 from the sequencing core
2. Regress out batch before mito QC
- [x] Push the branch
"#;

    fn project() -> Fixture {
        let fx = Fixture::new("project");
        fx.write("MYCELIUM.md", "# Mycelium\n")
            .write(".living/findings/batch-effects.md", BATCH_EFFECTS)
            .write(".living/findings/qc-thresholds.md", QC_THRESHOLDS)
            .write(".living/findings/exhaustion.md", EXHAUSTION)
            .write(
                ".living/findings/FINDINGS_REGISTRY.md",
                "# Findings Registry\n\n## F-999: not a topic\n",
            )
            .write(".living/findings/INDEX.md", "## F-998: not a topic\n")
            .write(".living/decisions.md", DECISIONS)
            .write(".living/learnings.md", LEARNINGS)
            .write("todo/TODO_REGISTRY.md", TODO_REGISTRY)
            .write(".mycelium/last-session.md", LAST_SESSION);
        fx
    }

    #[test]
    fn a_realistic_project_reads_every_section() {
        let fx = project();
        let k = read(fx.root());
        assert!(k.warnings.is_empty(), "{:?}", k.warnings);

        // Topics by slug; the registry/index files are not topics.
        let slugs: Vec<&str> = k.topics.iter().map(|t| t.slug.as_str()).collect();
        assert_eq!(slugs, ["batch-effects", "exhaustion", "qc-thresholds"]);
        let batch = &k.topics[0];
        assert_eq!(batch.path, ".living/findings/batch-effects.md");
        assert_eq!(
            batch.description,
            "How sequencing batch shifts expression estimates across runs."
        );
        let f3 = &batch.findings[0];
        assert_eq!(f3.id, "F-003");
        assert_eq!(f3.claim, "Batch 2 inflates the mitochondrial fraction");
        assert_eq!(f3.status, "supported");
        assert_eq!(f3.tags, ["qc", "batch", "mitochondria"]);
        assert_eq!(f3.ledger.len(), 2);
        assert!(f3.ledger.iter().all(|r| r.direction == "supports"));
        assert_eq!(f3.ledger[1].run, "run-22", "links unwrap to their text");
        assert_eq!(f3.ledger[1].dataset, "pbmc-20k");
        assert_eq!(f3.updated, "2026-09-10");
        assert_eq!(
            f3.questions,
            ["Does the shift persist after ambient RNA removal?"]
        );
        assert_eq!(f3.line, 11);
        assert!(f3.implications.starts_with("Regress out batch"));

        let exhaustion = &k.topics[1];
        let statuses: Vec<(&str, &str)> = exhaustion
            .findings
            .iter()
            .map(|f| (f.id.as_str(), f.status.as_str()))
            .collect();
        assert_eq!(statuses, [("F-001", "robust"), ("F-002", "contradicted")]);
        assert_eq!(exhaustion.findings[0].ledger[2].direction, "refines");
        assert_eq!(exhaustion.findings[1].ledger[1].direction, "contradicts");
        assert_eq!(exhaustion.findings[1].updated, "2026-08-30");

        // Numeric order, not file order; the `{…}` template question drops.
        let qc = &k.topics[2];
        assert_eq!(
            qc.description,
            "Per-sample QC cutoffs that hold across tissues."
        );
        let ids: Vec<&str> = qc.findings.iter().map(|f| f.id.as_str()).collect();
        assert_eq!(ids, ["F-004", "F-005"]);
        assert_eq!(qc.findings[0].status, "preliminary");
        assert_eq!(qc.findings[0].tags, ["doublets", "qc"]);
        assert_eq!(qc.findings[0].implications, "");
        assert_eq!(
            qc.findings[0].questions,
            ["Is the scaling linear above 20k cells?"]
        );
        assert_eq!(qc.findings[1].tags, ["qc", "thresholds"]);
        assert_eq!(qc.findings[1].status, "supported");

        // Decisions newest first.
        let titles: Vec<&str> = k.decisions.iter().map(|d| d.title.as_str()).collect();
        assert_eq!(
            titles,
            [
                "Drop sample S14 from the atlas",
                "Pin scanpy to 1.10",
                "Use scran size factors over CPM"
            ]
        );
        let scran = &k.decisions[2];
        assert_eq!(scran.date, "2026-08-02");
        assert_eq!(scran.line, 7);
        assert_eq!(
            scran.context,
            "Library sizes vary ten-fold across the two sequencing batches."
        );
        assert_eq!(scran.decision, "Normalise with scran pooled size factors.");
        assert_eq!(
            scran.alternatives,
            [
                "CPM — ignores composition bias",
                "SCTransform — too slow on 200k cells"
            ]
        );
        assert_eq!(
            scran.rationale,
            "scran corrects composition bias and scales to the atlas."
        );
        assert_eq!(
            scran.consequences,
            "Normalisation now needs a quick clustering pass first."
        );
        assert_eq!(scran.tags, ["normalisation", "scran"]);
        assert_eq!(
            scran.fp,
            fingerprint("decision", "2026-08-02", "Use scran size factors over CPM")
        );
        assert!(
            k.decisions[1].alternatives.is_empty(),
            "\"none\" is no alternative"
        );
        assert_eq!(k.decisions[1].tags, ["environment", "scanpy"]);
        assert_eq!(k.decisions[0].tags, ["qc", "exclusion"]);

        // Learnings newest first; template extras don't leak into fields.
        let categories: Vec<&str> = k.learnings.iter().map(|l| l.category.as_str()).collect();
        assert_eq!(
            categories,
            ["tip", "insight", "edge-case", "failure", "gotcha"]
        );
        let gotcha = &k.learnings[4];
        assert_eq!(gotcha.fp, "L-d407379e8c29");
        assert_eq!(
            gotcha.what,
            "`sc.pp.filter_genes` silently removed 1,204 genes before HVG selection."
        );
        assert_eq!(gotcha.why, "Marker panels lose genes without any warning.");
        assert_eq!(
            gotcha.resolution,
            "Filter after subsetting the marker panel."
        );
        assert_eq!(gotcha.tags, ["scanpy", "filtering"]);
        assert_eq!(
            k.learnings[3].what,
            "The doublet step ran out of memory on the 200k-cell object.\nIt died building the kNN graph."
        );
        assert_eq!(k.learnings[3].tags, ["slurm", "memory"]);

        // Todos: the registry table only (not the Status Key), including the
        // row mycelium's upsert appended below the marker.
        let todos: Vec<(&str, &str, &str)> = k
            .todos
            .iter()
            .map(|t| (t.item.as_str(), t.status.as_str(), t.file.as_str()))
            .collect();
        assert_eq!(
            todos,
            [
                (
                    "Compare with public datasets",
                    "open",
                    "todo/compare-public-data.md"
                ),
                (
                    "Re-run QC with MAD cutoffs",
                    "in-progress",
                    "todo/rerun-qc.md"
                ),
                ("Get batch 3 FASTQs", "blocked", ""),
                ("Write methods section", "complete", "todo/methods.md"),
            ]
        );
        assert_eq!(k.todos[0].priority, "idea");
        assert_eq!(k.todos[0].category, "validation");
        assert_eq!(k.todos[0].date, "2026-08-01");
        assert_eq!(k.todos[0].author, "Martin");

        // Open questions, flattened in topic order.
        let questions: Vec<(&str, &str)> = k
            .questions
            .iter()
            .map(|q| (q.finding.as_str(), q.text.as_str()))
            .collect();
        assert_eq!(
            questions,
            [
                ("F-003", "Does the shift persist after ambient RNA removal?"),
                ("F-002", "Is the melanoma association a cohort artefact?"),
                ("F-004", "Is the scaling linear above 20k cells?"),
            ]
        );

        let left = k.left_off.as_ref().expect("handoff");
        assert_eq!(left.path, ".mycelium/last-session.md");
        assert_eq!(
            left.worked_on,
            "- Re-ran QC on batches 1-2 with MAD cutoffs\n- Drafted the methods section"
        );
        assert_eq!(
            left.decisions,
            "- Dropped S14 (see .living/decisions.md for full context)"
        );
        assert_eq!(left.blockers, ["Batch 3 FASTQs still not delivered"]);
        assert_eq!(left.current, "- Branch: `qc-mad` | Tests: 42 passing");
        assert_eq!(
            left.next,
            [
                "Request batch 3 from the sequencing core",
                "Regress out batch before mito QC"
            ]
        );
        assert!(left.written_ms > 0);
        assert_eq!(left.session_id, None);

        assert_eq!(k.counts.findings, 5);
        assert_eq!(k.counts.decisions, 3);
        assert_eq!(k.counts.learnings, 5);
        // open + in-progress + blocked todos, plus three open questions.
        assert_eq!(k.counts.open, 6);
    }

    #[test]
    fn the_wire_shape_omits_absent_options() {
        let fx = project();
        let json = serde_json::to_value(read(fx.root())).unwrap();
        let keys: Vec<&str> = json
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        for key in [
            "left_off",
            "topics",
            "decisions",
            "learnings",
            "todos",
            "questions",
            "counts",
            "warnings",
        ] {
            assert!(keys.contains(&key), "missing {key}");
        }
        let left = json["left_off"].as_object().unwrap();
        assert!(!left.contains_key("session_id") && !left.contains_key("host"));
        assert_eq!(json["counts"]["open"], 6);
        assert_eq!(
            json["topics"][0]["findings"][0]["ledger"][0]["direction"],
            "supports"
        );

        let bare = Fixture::new("bare");
        let json = serde_json::to_value(read(bare.root())).unwrap();
        assert!(json.get("left_off").is_none());
        assert_eq!(json["decisions"], serde_json::json!([]));
    }

    #[test]
    fn fenced_and_commented_pseudo_entries_are_not_knowledge() {
        let fx = Fixture::new("fences");
        fx.write(
            ".living/decisions.md",
            r#"# Decisions

### [2026-05-01] Real decision

**Decision**: keep it

```markdown
### [2026-05-02] Example inside a fence
**Tags**: [nope]
```

**Tags**: [real]

~~~~
~~~
### [2026-05-03] A shorter closer doesn't close a longer fence
~~~~

- ```
### [2026-05-04] Fenced under a list marker
```

<!--
### [2026-05-05] Commented out
-->

### [2026-05-06] Second real decision
"#,
        );
        let k = read(fx.root());
        let titles: Vec<&str> = k.decisions.iter().map(|d| d.title.as_str()).collect();
        assert_eq!(titles, ["Second real decision", "Real decision"]);
        assert_eq!(k.decisions[1].tags, ["real"]);
        assert!(k.decisions[1].decision.starts_with("keep it"));
        assert!(k.warnings.is_empty(), "{:?}", k.warnings);
    }

    #[test]
    fn an_unclosed_fence_hides_the_rest_and_says_so() {
        let fx = Fixture::new("unclosed");
        fx.write(
            ".living/decisions.md",
            "### [2026-05-01] Before\n**Decision**: a\n\n```\n### [2026-05-02] After an unclosed fence\n",
        );
        let k = read(fx.root());
        assert_eq!(k.decisions.len(), 1);
        assert_eq!(k.decisions[0].title, "Before");
        assert!(
            k.warnings
                .iter()
                .any(|w| w.contains("unclosed code fence at line 4")),
            "{:?}",
            k.warnings
        );
    }

    #[test]
    fn dated_level_two_headings_are_legacy_entries_with_a_warning() {
        let fx = Fixture::new("legacy-level");
        fx.write(
            ".living/decisions.md",
            "# Decision Log\n\n## [2026-03-01] Legacy bracketed\n**Decision**: a\n\n\
             ## 2026-03-02 Legacy bare date\n**Decision**: b\n\n## Archive\n\n\
             Notes that are not an entry.\n\n### [2026-03-03] Current format\n**Decision**: c\n",
        );
        let k = read(fx.root());
        let got: Vec<(&str, &str, &str)> = k
            .decisions
            .iter()
            .map(|d| (d.date.as_str(), d.title.as_str(), d.decision.as_str()))
            .collect();
        assert_eq!(
            got,
            [
                ("2026-03-03", "Current format", "c"),
                ("2026-03-02", "Legacy bare date", "b"),
                ("2026-03-01", "Legacy bracketed", "a"),
            ]
        );
        assert_eq!(
            k.warnings,
            [".living/decisions.md: 2 entries use ## headings (mycelium 0.7 expects ###)"]
        );
    }

    #[test]
    fn undated_headings_sort_last_and_keep_dates_inside_the_title() {
        let fx = Fixture::new("undated");
        fx.write(
            ".living/learnings.md",
            "### Untitled lesson\n**Category**: tip\n**What happened**: x\n\n\
             ### Cohort 2026-01-01 to 2026-02-01 mislabeled\n**Category**: gotcha\n\n\
             ### [2026-02-10] Dated one\n**Category**: insight\n",
        );
        let k = read(fx.root());
        let got: Vec<(&str, &str)> = k
            .learnings
            .iter()
            .map(|l| (l.date.as_str(), l.title.as_str()))
            .collect();
        assert_eq!(
            got,
            [
                ("2026-02-10", "Dated one"),
                ("", "Cohort 2026-01-01 to 2026-02-01 mislabeled"),
                ("", "Untitled lesson"),
            ]
        );
        assert_eq!(k.learnings[2].category, "tip");
        assert_eq!(k.learnings[2].what, "x");
        assert_eq!(
            k.learnings[2].fp,
            fingerprint("learning", "", "Untitled lesson")
        );
    }

    #[test]
    fn colon_inside_the_bold_and_plain_labels_parse() {
        let fx = Fixture::new("variants");
        fx.write(
            ".living/learnings.md",
            "### [2026-06-01] Colon inside the bold\n\
             **Category:** Edge case\n\
             **What happened:** First line\ncontinues here\n\n\n\nand a second paragraph.\n\
             **Why it matters:** because\n\
             - **Resolution**: bulleted known label\n\
             Tags: plain, list\n",
        );
        let k = read(fx.root());
        let l = &k.learnings[0];
        assert_eq!(l.category, "edge-case");
        assert_eq!(
            l.what,
            "First line\ncontinues here\n\nand a second paragraph."
        );
        assert_eq!(l.why, "because");
        assert_eq!(l.resolution, "bulleted known label");
        assert_eq!(l.tags, ["plain", "list"]);
    }

    #[test]
    fn an_oversized_file_is_skipped_with_a_warning() {
        let fx = Fixture::new("oversized");
        let mut huge = String::from("### [2026-01-01] Buried\n**Category**: tip\n");
        huge.push_str(&"x".repeat(MAX_FILE_BYTES as usize));
        fx.write(".living/learnings.md", &huge).write(
            ".living/decisions.md",
            "### [2026-01-02] Small\n**Decision**: ok\n",
        );
        let k = read(fx.root());
        assert!(k.learnings.is_empty());
        assert_eq!(k.decisions.len(), 1);
        assert_eq!(k.warnings.len(), 1);
        assert!(
            k.warnings[0]
                .starts_with(".living/learnings.md: skipped; 2.1 MiB is over the 2 MiB read cap"),
            "{:?}",
            k.warnings
        );
        // Still stamped: growing or shrinking it must invalidate a cache.
        assert!(stamp(fx.root())
            .files
            .iter()
            .any(|(rel, ..)| rel == ".living/learnings.md"));
    }

    #[test]
    fn a_workspace_without_mycelium_reads_nothing() {
        let fx = Fixture::new("none");
        // A stray registry is not knowledge without `.living` / MYCELIUM.md.
        fx.write("todo/TODO_REGISTRY.md", TODO_REGISTRY);
        let k = read(fx.root());
        assert!(k.left_off.is_none());
        assert!(k.topics.is_empty() && k.decisions.is_empty() && k.learnings.is_empty());
        assert!(k.todos.is_empty() && k.questions.is_empty() && k.warnings.is_empty());
        assert_eq!(k.counts.open, 0);
        assert_eq!(stamp(fx.root()), Stamp::default());
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_never_followed() {
        use std::os::unix::fs::symlink;
        let outside = Fixture::new("outside");
        outside.write(".living/decisions.md", DECISIONS);
        outside.write("topic.md", BATCH_EFFECTS);
        let fx = Fixture::new("symlinked");
        symlink(outside.root().join(".living"), fx.root().join(".living")).unwrap();

        let k = read(fx.root());
        assert!(k.decisions.is_empty());
        assert_eq!(k.warnings.len(), 1);
        assert!(k.warnings[0].starts_with(".living is a symlink; not followed"));

        // With MYCELIUM.md it is a mycelium workspace — the link still isn't
        // followed, and neither is a linked topic file inside a real .living.
        fx.write("MYCELIUM.md", "# Mycelium\n");
        assert!(read(fx.root()).decisions.is_empty());

        let real = Fixture::new("real-with-link");
        real.write(".living/findings/exhaustion.md", EXHAUSTION);
        symlink(
            outside.root().join("topic.md"),
            real.root().join(".living/findings/linked.md"),
        )
        .unwrap();
        let k = read(real.root());
        let slugs: Vec<&str> = k.topics.iter().map(|t| t.slug.as_str()).collect();
        assert_eq!(slugs, ["exhaustion"]);
        assert!(k.warnings[0].starts_with(".living/findings/linked.md is a symlink"));
        assert_eq!(stamp(real.root()).refused, [".living/findings/linked.md"]);
    }

    #[test]
    fn fingerprints_are_stable_and_normalized() {
        // FNV-1a 64 reference values, computed independently (Python).
        assert_eq!(
            fingerprint(
                "learning",
                "2026-08-02",
                "Scanpy drops genes with zero counts"
            ),
            "L-d407379e8c29"
        );
        assert_eq!(fingerprint("decision", "", "Untitled"), "D-9c35de877e4d");
        assert_eq!(
            fingerprint(
                "Learning",
                " 2026-08-02 ",
                "SCANPY  drops\tgenes with zero counts\n"
            ),
            "L-d407379e8c29"
        );
        let title = "Scanpy drops genes with zero counts";
        assert_ne!(
            fingerprint("decision", "2026-08-02", title)[2..],
            fingerprint("learning", "2026-08-02", title)[2..]
        );
        assert_ne!(
            fingerprint("learning", "2026-08-03", title),
            "L-d407379e8c29"
        );
    }

    #[test]
    fn verbatim_duplicate_entries_get_distinct_ids() {
        let fx = Fixture::new("dupes");
        fx.write(
            ".living/learnings.md",
            "### [2026-01-01] Same\n**What happened**: first\n\n\
             ### [2026-01-01] Same\n**What happened**: second\n",
        );
        let k = read(fx.root());
        let base = fingerprint("learning", "2026-01-01", "Same");
        // Newest (later in file) first; the first-written keeps the bare id.
        assert_eq!(k.learnings[0].what, "second");
        assert_eq!(k.learnings[0].fp, format!("{base}~2"));
        assert_eq!(k.learnings[1].fp, base);
    }

    #[test]
    fn caps_cut_text_on_char_boundaries() {
        let capped = cap_text("é".repeat(2000));
        assert!(capped.len() <= MAX_TEXT_BYTES);
        assert!(capped.ends_with('…'));
        assert!(capped.trim_end_matches('…').chars().all(|c| c == 'é'));
        assert_eq!(cap_text("short".to_owned()), "short");

        let fx = Fixture::new("caps");
        let tags: Vec<String> = (0..30).map(|i| format!("t{i}")).collect();
        fx.write(
            ".living/decisions.md",
            &format!(
                "### [2026-01-01] Long\n**Context**: {}\n**Tags**: {}\n",
                "日".repeat(1000),
                tags.join(", ")
            ),
        );
        let d = &read(fx.root()).decisions[0];
        assert!(d.context.len() <= MAX_TEXT_BYTES && d.context.ends_with('…'));
        assert!(d.context.trim_end_matches('…').chars().all(|c| c == '日'));
        assert_eq!(d.tags.len(), MAX_TAGS);
        assert_eq!(d.tags[19], "t19");
    }

    #[test]
    fn stamp_changes_when_a_file_changes_or_appears() {
        let fx = project();
        let first = stamp(fx.root());
        assert_eq!(first, stamp(fx.root()));
        assert!(first
            .files
            .iter()
            .any(|(rel, ..)| rel == ".mycelium/last-session.md"));

        let path = fx.root().join(".living/learnings.md");
        let mut body = std::fs::read_to_string(&path).unwrap();
        body.push_str("\n### [2026-09-20] Appended\n**Category**: tip\n");
        std::fs::write(&path, body).unwrap();
        let appended = stamp(fx.root());
        assert_ne!(first, appended);
        assert_eq!(read(fx.root()).learnings[0].title, "Appended");

        fx.write(
            ".living/findings/new-topic.md",
            "## F-010: New\n**Status:** preliminary\n",
        );
        assert_ne!(appended, stamp(fx.root()));
    }

    #[test]
    fn an_in_flight_run_handoff_is_the_fallback() {
        let fx = Fixture::new("run-handoff");
        let body = "## What was worked on\n- {}\n\n\
                    ## Current state\n- Stop hook finalization pending\n- Loader fixed\n";
        fx.write(".living/learnings.md", "")
            .write(
                ".mycelium/run/claude/sess-a/last-session.md",
                &body.replace("{}", "older session"),
            )
            .write(
                ".mycelium/run/codex/sess-b/last-session.md",
                &body.replace("{}", "newer session"),
            )
            .write(
                ".mycelium/run/codex/bad name/last-session.md",
                &body.replace("{}", "not a mycelium run dir"),
            );
        fx.set_mtime(".mycelium/run/claude/sess-a/last-session.md", 600);
        fx.set_mtime(".mycelium/run/codex/sess-b/last-session.md", 60);
        fx.set_mtime(".mycelium/run/codex/bad name/last-session.md", 0);

        let left = read(fx.root()).left_off.expect("run handoff");
        assert_eq!(left.worked_on, "- newer session");
        assert_eq!(left.session_id.as_deref(), Some("sess-b"));
        assert_eq!(left.host.as_deref(), Some("codex"));
        assert_eq!(left.path, ".mycelium/run/codex/sess-b/last-session.md");
        // mycelium's finalizer strips lifecycle chatter; so do we.
        assert_eq!(left.current, "- Loader fixed");

        // An accepted shared handoff always wins.
        fx.write(".mycelium/last-session.md", LAST_SESSION);
        let left = read(fx.root()).left_off.unwrap();
        assert_eq!(left.path, ".mycelium/last-session.md");
        assert_eq!(left.session_id, None);
    }

    #[test]
    fn the_alternate_handoff_schema_and_fallback_filler() {
        let fx = Fixture::new("alt-handoff");
        fx.write(".living/decisions.md", "").write(
            ".mycelium/last-session.md",
            "## Current State\nParser half done.\n\n\
             ## What Was Done\n\
             - Completed the session work recorded in the finalized session log.\n\
             - Fixed the loader\n\n\
             ## Key Decisions\n\
             - See `.living/decisions.md` for decisions recorded during this session.\n\n\
             ## Next Steps\nShip the loader fix, then rerun QC.\n\n\
             ## Relevant Files\n- src/loader.py\n",
        );
        let left = read(fx.root()).left_off.unwrap();
        assert_eq!(left.current, "Parser half done.");
        assert_eq!(left.worked_on, "- Fixed the loader");
        assert_eq!(left.decisions, "");
        assert!(left.blockers.is_empty());
        assert_eq!(left.next, ["Ship the loader fix, then rerun QC."]);

        // Nothing but filler is no handoff at all.
        fx.write(
            ".mycelium/last-session.md",
            "## Next steps\n\
             - Review the finalized session log and continue from the current branch state.\n",
        );
        let k = read(fx.root());
        assert!(k.left_off.is_none());
        assert!(k.warnings[0].contains("no handoff sections with content"));
    }

    #[test]
    fn the_legacy_todolist_is_read_with_a_warning() {
        let fx = Fixture::new("todolist");
        fx.write("MYCELIUM.md", "").write(
            "todo/TODOLIST.md",
            "# Todo List\n\n## Items\n\n\
             <!-- Add todo items below. Link to detailed writeups as needed. -->\n\
             - [ ] Compare with GTEx ([notes](gtex.md))\n- [x] Draft figure 2\n- Plain item\n",
        );
        let k = read(fx.root());
        let got: Vec<(&str, &str, &str)> = k
            .todos
            .iter()
            .map(|t| (t.item.as_str(), t.status.as_str(), t.file.as_str()))
            .collect();
        assert_eq!(
            got,
            [
                ("Compare with GTEx (notes)", "open", "todo/gtex.md"),
                ("Draft figure 2", "complete", ""),
                ("Plain item", "open", ""),
            ]
        );
        assert_eq!(k.counts.open, 2);
        assert!(k.warnings[0].contains("legacy todo list"));
    }

    #[test]
    fn the_ledger_tolerates_reordered_and_missing_columns() {
        let rows = parse_ledger(&[
            "| Direction | Date | Result |",
            "|---|---|---|",
            "| Refines the claim | 2026-01-02 | narrower |",
            "| supports | 2026-01-03 |",
            "| contradicts |",
        ])
        .rows;
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].direction, "refines");
        assert_eq!(rows[0].date, "2026-01-02");
        assert_eq!(rows[0].result, "narrower");
        assert_eq!(rows[0].run, "");
        assert_eq!(rows[1].direction, "supports");
        assert_eq!(rows[1].result, "");
        assert_eq!(
            (rows[2].date.as_str(), rows[2].direction.as_str()),
            ("", "contradicts")
        );
        // Positional fallback without a header; the template row drops.
        let rows = parse_ledger(&[
            "| 2026-02-01 | r1 | d | p | ok | contradicts |",
            "| YYYY-MM-DD | x |",
        ])
        .rows;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].run, "r1");
        assert_eq!(rows[0].direction, "contradicts");
    }

    #[test]
    fn a_long_ledger_keeps_the_newest_rows_but_dates_from_all() {
        let lines: Vec<String> = (1..=60)
            .map(|n| {
                let date = if n == 3 { "2027-06-01" } else { "2026-01-01" };
                format!("| {date} | r{n} | d | p | ok | supports |")
            })
            .collect();
        let lines: Vec<&str> = lines.iter().map(String::as_str).collect();
        let ledger = parse_ledger(&lines);
        assert_eq!(ledger.total, 60);
        assert_eq!(ledger.rows.len(), MAX_LEDGER_ROWS);
        assert_eq!(ledger.rows[0].run, "r11");
        assert_eq!(ledger.rows[MAX_LEDGER_ROWS - 1].run, "r60");
        // The newest date lives on a row that was not kept.
        assert_eq!(ledger.newest.as_deref(), Some("2027-06-01"));
    }

    #[test]
    fn the_fence_port_matches_markdown_fences() {
        let four = opening_fence("````").unwrap();
        assert!(!closes("```", &four), "a shorter run is content");
        assert!(closes("````", &four) && closes("`````  ", &four));
        assert!(!closes("~~~~", &four), "the other fence character");
        assert!(!closes("```` trailing", &four));

        assert!(
            opening_fence("    ```").is_none(),
            "four spaces is indented code"
        );
        assert!(opening_fence("\t```").is_none(), "a tab reaches column 4");
        assert!(opening_fence("```rust").is_some());
        assert!(opening_fence("``` a`b").is_none(), "backtick info string");
        assert!(opening_fence("~~~ a`b").is_some());
        assert!(opening_fence("``").is_none());

        let quoted = opening_fence("> ```").unwrap();
        assert!(!closes("```", &quoted) && closes("> ```", &quoted));

        let listed = opening_fence("1. ~~~").unwrap();
        assert_eq!(listed.column, 3);
        assert!(closes("~~~", &listed) && closes("      ~~~", &listed));
        assert!(
            !closes("       ~~~", &listed),
            "past the three-column slack"
        );
        assert!(!closes("\t\t~~~", &listed));
    }

    #[test]
    fn todo_links_resolve_inside_the_workspace() {
        assert_eq!(todo_path("item.md"), "todo/item.md");
        assert_eq!(todo_path("./sub/item.md#notes"), "todo/sub/item.md");
        assert_eq!(todo_path("../analysis/qc/QC.md"), "analysis/qc/QC.md");
        assert_eq!(todo_path("../../etc/passwd"), "../../etc/passwd");
        assert_eq!(todo_path("https://example.org/x"), "https://example.org/x");
        assert_eq!(todo_path("/abs/path.md"), "/abs/path.md");
        assert_eq!(
            strip_links("see ![fig](f.png) and [a](b.md)"),
            ("see fig and a".to_owned(), Some("f.png".to_owned()))
        );
    }

    #[test]
    fn an_unclosed_comment_warns_but_a_code_span_opener_is_text() {
        let fx = Fixture::new("comments");
        fx.write(
            ".living/learnings.md",
            "### [2026-01-01] About `<!--` in markdown\n**What happened**: see title\n\n\
             ### [2026-01-02] Still visible\n**What happened**: yes\n\n\
             <!-- a note that never closes\n### [2026-01-03] Hidden\n",
        );
        let k = read(fx.root());
        let titles: Vec<&str> = k.learnings.iter().map(|l| l.title.as_str()).collect();
        assert_eq!(titles, ["Still visible", "About `<!--` in markdown"]);
        assert_eq!(
            k.warnings,
            [".living/learnings.md: unclosed HTML comment at line 7; nothing after it was read as knowledge"]
        );
    }

    #[test]
    fn the_finding_cap_spends_topics_in_slug_order_and_skips_the_rest() {
        let topic = |first: usize, n: usize| {
            (first..first + n)
                .map(|id| format!("## F-{id:04}: claim {id}\n**Status:** supported\n\n"))
                .collect::<String>()
        };
        let fx = Fixture::new("finding-cap");
        fx.write(".living/findings/b.md", &topic(601, 600))
            .write(".living/findings/a.md", &topic(1, 600))
            .write(".living/findings/c.md", &topic(1201, 10));
        let k = read(fx.root());
        let per_topic: Vec<(&str, usize)> = k
            .topics
            .iter()
            .map(|t| (t.slug.as_str(), t.findings.len()))
            .collect();
        assert_eq!(per_topic, [("a", 600), ("b", 400)]);
        assert_eq!(k.topics[1].findings[399].id, "F-1000");
        assert_eq!(k.counts.findings, MAX_ENTRIES as u32);
        assert_eq!(
            k.warnings,
            ["findings: showing the first 1000 of 1200 (1 more topic files not read)"]
        );
    }

    #[test]
    fn a_log_of_bare_headings_keeps_only_the_newest() {
        let log: String = (0..MAX_SCANNED_ENTRIES + 3)
            .map(|i| format!("### [2026-01-01] E{i}\n"))
            .collect();
        let fx = Fixture::new("scan-cap");
        fx.write(".living/decisions.md", &log);
        let k = read(fx.root());
        assert_eq!(k.decisions.len(), MAX_ENTRIES);
        // Same date: later in the file is newer.
        assert_eq!(
            k.decisions[0].title,
            format!("E{}", MAX_SCANNED_ENTRIES + 2)
        );
        assert_eq!(
            k.warnings,
            [
                format!(
                    ".living/decisions.md: {} entry headings; only the last {MAX_SCANNED_ENTRIES} were considered",
                    MAX_SCANNED_ENTRIES + 3
                ),
                format!(
                    ".living/decisions.md: showing the newest {MAX_ENTRIES} of {MAX_SCANNED_ENTRIES} entries"
                ),
            ]
        );
    }

    #[test]
    fn the_per_read_byte_budget_drops_topic_files_first() {
        let fx = Fixture::new("byte-budget");
        let filler = "x".repeat(1900 * 1024);
        fx.write("todo/TODO_REGISTRY.md", TODO_REGISTRY);
        for n in 0..9 {
            fx.write(
                &format!(".living/findings/t{n}.md"),
                &format!("## F-{n}: claim\n**Status:** robust\n\n{filler}\n"),
            );
        }
        let k = read(fx.root());
        // 8 × ~1.9 MiB fit in 16 MiB; the ninth doesn't. Todos, planned
        // before topics, are never the casualty.
        assert_eq!(k.topics.len(), 8);
        assert_eq!(k.todos.len(), 4);
        assert_eq!(
            k.warnings,
            ["1 knowledge files not read: the 16 MiB per-read budget was spent"]
        );
    }

    #[test]
    fn stale_stop_chatter_matches_the_finalizer() {
        assert!(is_stale_stop_line("- Stop hook finalization pending"));
        assert!(is_stale_stop_line("Natural stop not yet reached"));
        assert!(is_stale_stop_line("attempting a natural stop"));
        assert!(!is_stale_stop_line(
            "- Stop the Slurm array before rerunning"
        ));
        assert!(!is_stale_stop_line("Pending: batch 3 FASTQs"));
        // Linear on a hostile line: many leads, no tail.
        assert!(!is_stale_stop_line(&"stop hook ".repeat(200_000)));
    }

    #[test]
    fn a_none_with_a_reason_is_not_an_open_question() {
        for none in [
            "None. The claim holds by the definition of the successor function.",
            "None",
            "**None** — resolved in F-002",
            "No open questions: the ledger is complete.",
            "N/A (single dataset)",
        ] {
            assert!(says_none(none), "{none}");
        }
        for real in [
            "None of the samples replicate — why?",
            "Nonetheless, does lane 4 differ?",
            "Does 200 UMIs let ambient RNA in?",
        ] {
            assert!(!says_none(real), "{real}");
        }
    }

    /// The layout real repositories write: addenda as `###`/`####`
    /// sub-headings carrying the finding's id, one filed under the wrong
    /// finding, a reused id, and addenda with no finding in the file.
    const ADDENDA: &str = r#"---
topic: data-completeness
description: Which cohorts' data are complete and ready.
last_updated: 2026-07-21
---

# Data completeness

## F-027: Missing-data forensics across all cohorts (2026-07-15) — CUIMC2 is COMPLETE …
**Status:** supported
**Claim:** Every cohort's raw data is accounted for.
**Implications:** Nothing needs re-requesting from CUIMC.
**Tags:** f-027, data-completeness

### Evidence Ledger
| Date | Run/Session | Dataset | Project | Result | Direction |
|---|---|---|---|---|---|
| 2026-07-15 | run-40 | cuimc2 | pd | every cohort accounted for | supports |

### F-027 addendum: CUIMC2 genotype readiness — 238/240 donors covered …
Two donors lack genotype calls; both are re-queued.
**Tags:** genotyping, f-027

#### F-027 addendum: bulk DLPFC reads are PRESENT …
The bulk DLPFC FASTQs were on the archive tier, not missing.

### F-027 addendum (2): CUIMC2 demux pileup bottleneck is the BAM READ COUNT …
**Status:** robust
Pileup time tracks reads per BAM, not donor count.

#### Evidence
| Date | Run/Session | Dataset | Project | Result | Direction |
|---|---|---|---|---|---|
| 2026-07-20 | run-44 | cuimc2 | pd | pileup time tracks reads | refines |

#### Open Questions
- Would downsampling BAMs keep demux accuracy?

## F-028: Sample swaps are rare
**Status:** preliminary

#### F-028 Addendum — one swap found in batch 3
A single swap, fixed at the source.

### Evidence Ledger
| Date | Run/Session | Dataset | Project | Result | Direction |
|---|---|---|---|---|---|
| 2026-07-18 | run-42 | batch-3 | pd | one swap in 96 | supports |

### F-027 addendum (3): late
Filed under F-028 by mistake.

## F-038: Ambient RNA is lane-specific
**Status:** robust

## F-038: Ambient RNA correction changes marker calls
**Status:** preliminary

### F-038 addendum: holds with SoupX too
Checked with SoupX 1.6.

## F-041 addendum: an addendum with no finding in this file
**Status:** preliminary
A stray note.

### F-041 addendum (2): still stray
**Status:** robust
Another.

## F-050: Addendum-free protocols replicate
**Status:** robust

### F-060 addendum (early): written above its finding
**Tags**:
- early
- f-060

Seen first in the pilot.

## F-051: Addendum to protocol v2 improves yield
**Status:** preliminary
**Implications:** Switch the pilot to v2.

## F-060: Pilot libraries replicate
**Status:** supported
"#;

    #[test]
    fn an_addendum_is_part_of_its_finding() {
        let fx = Fixture::new("addenda");
        fx.write(".living/findings/data-completeness.md", ADDENDA);
        let k = read(fx.root());
        let line_of = |start: &str| {
            let at = ADDENDA.split('\n').position(|l| l.starts_with(start));
            line_no(at.unwrap_or_else(|| panic!("no line {start:?}")))
        };
        let findings = &k.topics[0].findings;
        let ids: Vec<&str> = findings.iter().map(|f| f.id.as_str()).collect();
        // One finding per id, except the id written on two different findings.
        assert_eq!(
            ids,
            ["F-027", "F-028", "F-038", "F-038", "F-041", "F-050", "F-051", "F-060"]
        );
        assert_eq!(k.counts.findings, 8);

        let listed = |f: &Finding| -> Vec<(String, String, String, u32)> {
            f.addenda
                .iter()
                .map(|a| (a.label.clone(), a.title.clone(), a.text.clone(), a.line))
                .collect()
        };
        let entry = |label: &str, title: &str, text: &str, line: u32| {
            (label.to_owned(), title.to_owned(), text.to_owned(), line)
        };

        // Every addendum belongs to the latest finding above it with its id,
        // wherever it was filed, and is listed in file order.
        let f27 = &findings[0];
        assert_eq!(
            f27.claim,
            "Missing-data forensics across all cohorts (2026-07-15) — CUIMC2 is COMPLETE …"
        );
        assert_eq!(f27.line, line_of("## F-027"));
        assert_eq!(f27.implications, "Nothing needs re-requesting from CUIMC.");
        assert_eq!(
            listed(f27),
            [
                entry(
                    "Addendum",
                    "CUIMC2 genotype readiness — 238/240 donors covered …",
                    "Two donors lack genotype calls; both are re-queued.",
                    line_of("### F-027 addendum: CUIMC2"),
                ),
                entry(
                    "Addendum",
                    "bulk DLPFC reads are PRESENT …",
                    "The bulk DLPFC FASTQs were on the archive tier, not missing.",
                    line_of("#### F-027 addendum"),
                ),
                entry(
                    "Addendum (2)",
                    "CUIMC2 demux pileup bottleneck is the BAM READ COUNT …",
                    "Pileup time tracks reads per BAM, not donor count.",
                    line_of("### F-027 addendum (2)"),
                ),
                entry(
                    "Addendum (3)",
                    "late",
                    "Filed under F-028 by mistake.",
                    line_of("### F-027 addendum (3)"),
                ),
            ]
        );
        // The finding's own status stands over an addendum's; the addenda's
        // tags, evidence and questions are the finding's.
        assert_eq!(f27.status, "supported");
        assert_eq!(f27.tags, ["f-027", "data-completeness", "genotyping"]);
        let rows: Vec<(&str, &str)> = f27
            .ledger
            .iter()
            .map(|r| (r.date.as_str(), r.direction.as_str()))
            .collect();
        assert_eq!(
            rows,
            [("2026-07-15", "supports"), ("2026-07-20", "refines")]
        );
        assert_eq!(f27.updated, "2026-07-20");
        assert_eq!(
            f27.questions,
            ["Would downsampling BAMs keep demux accuracy?"]
        );

        // A `####` addendum ends at the finding's next `###` section, which
        // stays the finding's own.
        let f28 = &findings[1];
        assert_eq!(f28.status, "preliminary");
        assert_eq!(f28.implications, "");
        assert_eq!(
            listed(f28),
            [entry(
                "Addendum",
                "one swap found in batch 3",
                "A single swap, fixed at the source.",
                line_of("#### F-028"),
            )]
        );
        assert_eq!(f28.ledger.len(), 1);
        assert_eq!(f28.ledger[0].result, "one swap in 96");

        // A reused id: two findings, never merged; the addendum extends the
        // later one.
        let (a, b) = (&findings[2], &findings[3]);
        assert_eq!(
            (a.claim.as_str(), a.status.as_str(), a.addenda.len()),
            ("Ambient RNA is lane-specific", "robust", 0)
        );
        assert_eq!(
            (b.claim.as_str(), b.status.as_str()),
            ("Ambient RNA correction changes marker calls", "preliminary")
        );
        assert_eq!(
            listed(b),
            [entry(
                "Addendum",
                "holds with SoupX too",
                "Checked with SoupX 1.6.",
                line_of("### F-038 addendum"),
            )]
        );

        // Addenda with no finding here: the first stands in for it (its
        // heading the claim), all are listed; with no stated status of its
        // own, the newest addendum's.
        let f41 = &findings[4];
        assert_eq!(
            f41.claim,
            "addendum: an addendum with no finding in this file"
        );
        assert_eq!(f41.line, line_of("## F-041"));
        assert_eq!(f41.status, "robust");
        assert_eq!(
            listed(f41),
            [
                entry(
                    "Addendum",
                    "an addendum with no finding in this file",
                    "A stray note.",
                    line_of("## F-041"),
                ),
                entry(
                    "Addendum (2)",
                    "still stray",
                    "Another.",
                    line_of("### F-041 addendum (2)"),
                ),
            ]
        );

        // Claims that open with the word are findings.
        assert_eq!(findings[5].claim, "Addendum-free protocols replicate");
        let f51 = &findings[6];
        assert_eq!(
            (f51.claim.as_str(), f51.implications.as_str()),
            (
                "Addendum to protocol v2 improves yield",
                "Switch the pilot to v2."
            )
        );

        // An addendum written above its finding is still its own; its tag
        // bullets are tags, not text.
        let f60 = &findings[7];
        assert_eq!(f60.claim, "Pilot libraries replicate");
        assert_eq!(f60.line, line_of("## F-060"));
        assert_eq!(f60.tags, ["early", "f-060"]);
        assert_eq!(
            listed(f60),
            [entry(
                "Addendum (early)",
                "written above its finding",
                "Seen first in the pilot.",
                line_of("### F-060 addendum"),
            )]
        );

        // On the wire, only a finding with addenda carries the key.
        let wire = |f: &Finding| serde_json::to_value(f).unwrap();
        assert_eq!(wire(f27)["addenda"][1]["label"], "Addendum");
        assert!(wire(&findings[5]).get("addenda").is_none());

        // A reused id is two findings with two keys, and one Tidy up row —
        // not a warning.
        let keys: Vec<&str> = findings.iter().map(|f| f.key.as_str()).collect();
        assert_eq!(
            keys,
            [
                "data-completeness/F-027",
                "data-completeness/F-028",
                "data-completeness/F-038",
                "data-completeness/F-038~2",
                "data-completeness/F-041",
                "data-completeness/F-050",
                "data-completeness/F-051",
                "data-completeness/F-060"
            ]
        );
        let rel = ".living/findings/data-completeness.md";
        assert_eq!(
            k.warnings,
            [format!(
                "{rel}: the F-041 addendum at line {} has no F-041 finding in this file; shown on its own",
                line_of("## F-041"),
            )]
        );
        assert_eq!(k.tidy.len(), 1);
        assert_eq!(k.tidy[0].kind, "duplicate-id");
        assert_eq!(
            k.tidy[0].text,
            "1 finding id names more than one finding: F-038."
        );
        assert!(k.tidy[0].ask.contains(&format!(
            "F-038 ({rel}:{}, {rel}:{})",
            line_of("## F-038: Ambient RNA is"),
            line_of("## F-038: Ambient RNA correction")
        )));
    }

    #[test]
    fn addendum_headings_and_their_bounds() {
        let lead = |rest: &str| followup_heading(rest).map(|f| (f.label, f.title.to_owned()));
        let some = |label: &str, title: &str| Some((label.to_owned(), title.to_owned()));
        assert_eq!(lead("addendum: a — b"), some("Addendum", "a — b"));
        assert_eq!(lead("Addendum (2): x"), some("Addendum (2)", "x"));
        assert_eq!(lead("addendum (12:30) — x"), some("Addendum (12:30)", "x"));
        assert_eq!(
            lead("addendum 2026-07-20: x"),
            some("Addendum 2026-07-20", "x")
        );
        assert_eq!(lead("addendum**: x"), some("Addendum", "x"));
        assert_eq!(lead("ADDENDA"), some("ADDENDA", ""));
        assert_eq!(lead("addendum - x"), some("Addendum", "x"));
        assert_eq!(lead("addendum 2 - x"), some("Addendum 2", "x"));
        assert_eq!(lead("addendum (late)"), some("Addendum (late)", ""));
        for claim in [
            "addendum on reads",
            "Addendum to protocol v2 improves yield",
            "addendum (unclosed: x",
            "Addendum-free protocols",
            "Addendums pile up",
            "addendumx",
            "An addendum",
        ] {
            assert_eq!(lead(claim), None, "{claim}");
        }

        // Many long addenda: the newest are kept, each text capped.
        let long = "word ".repeat(1000);
        let file: String = std::iter::once("## F-001: claim\n**Status:** supported\n".to_owned())
            .chain(
                (0..MAX_ADDENDA + 10).map(|n| format!("### F-001 addendum ({n}): note\n{long}\n")),
            )
            .collect();
        let fx = Fixture::new("addenda-cap");
        fx.write(".living/findings/t.md", &file);
        let k = read(fx.root());
        assert_eq!(k.topics[0].findings.len(), 1);
        let f = &k.topics[0].findings[0];
        assert_eq!(f.addenda.len(), MAX_ADDENDA);
        assert_eq!(f.addenda[0].label, "Addendum (10)");
        assert!(f
            .addenda
            .iter()
            .all(|a| a.text.len() <= MAX_TEXT_BYTES && a.text.ends_with('…')));
        assert_eq!(
            k.warnings,
            [format!(
                "F-001: showing the newest {MAX_ADDENDA} of {} addenda",
                MAX_ADDENDA + 10
            )]
        );
    }

    /// Counts over any tree, for checking the reader against a real
    /// project without copying it anywhere: `MYCELIUM_TREE=/path/to/project
    /// cargo test --release probe -- --ignored --nocapture`. Prints counts,
    /// kinds and timings only, never content.
    #[test]
    #[ignore = "reads MYCELIUM_TREE"]
    fn probe_a_real_tree() {
        let Some(root) = std::env::var_os("MYCELIUM_TREE") else {
            return;
        };
        let started = std::time::Instant::now();
        let k = read(Path::new(&root));
        let took = started.elapsed();
        let json = serde_json::to_vec(&k).unwrap();
        // MYCELIUM_JSON=<file> also writes the snapshot there.
        if let Some(out) = std::env::var_os("MYCELIUM_JSON") {
            std::fs::write(out, &json).unwrap();
        }
        let findings: Vec<&Finding> = k.topics.iter().flat_map(|t| &t.findings).collect();
        let keys: HashSet<&str> = findings.iter().map(|f| f.key.as_str()).collect();
        let ids: HashSet<&str> = findings.iter().map(|f| f.id.as_str()).collect();
        let n = |pred: &dyn Fn(&Finding) -> bool| findings.iter().filter(|f| pred(f)).count();
        println!("read in {took:?}; snapshot {} bytes", json.len());
        println!(
            "findings {} (unique keys {}, unique ids {}); stated {}, status known {}, dated {}, refs {}, cites {}, state {}, amends {}, statement {}",
            findings.len(),
            keys.len(),
            ids.len(),
            n(&|f| !f.stated.is_empty()),
            n(&|f| f.status != "unknown"),
            n(&|f| !f.date.is_empty()),
            n(&|f| !f.refs.is_empty()),
            n(&|f| !f.cites.is_empty()),
            n(&|f| f.state.is_some()),
            n(&|f| !f.amends.is_empty()),
            n(&|f| !f.statement.is_empty()),
        );
        let mut states: HashMap<&str, usize> = HashMap::new();
        for f in &findings {
            if let Some(s) = &f.state {
                *states.entry(s.kind).or_default() += 1;
            }
        }
        let mut kinds: HashMap<&str, usize> = HashMap::new();
        for a in findings.iter().flat_map(|f| &f.addenda) {
            *kinds.entry(a.kind).or_default() += 1;
        }
        println!("finding states {states:?}; follow-ups {kinds:?}");
        let mut cite_kinds: HashMap<&str, usize> = HashMap::new();
        for c in findings.iter().flat_map(|f| &f.cites) {
            *cite_kinds.entry(c.kind).or_default() += 1;
        }
        println!("finding cites by kind {cite_kinds:?}");
        let d = &k.decisions;
        println!(
            "decisions {}: with id {}, dated {}, titled {}, stated {}, state {}, amends {}, refs {}, empty decision field {}",
            d.len(),
            d.iter().filter(|x| !x.id.is_empty()).count(),
            d.iter().filter(|x| !x.date.is_empty()).count(),
            d.iter().filter(|x| !x.title.is_empty()).count(),
            d.iter().filter(|x| !x.stated.is_empty()).count(),
            d.iter().filter(|x| x.state.is_some()).count(),
            d.iter().filter(|x| !x.amends.is_empty()).count(),
            d.iter().filter(|x| !x.refs.is_empty()).count(),
            d.iter().filter(|x| x.decision.is_empty()).count(),
        );
        let l = &k.learnings;
        println!(
            "learnings {}: with id {}, dated {}, what {}, why {}, resolution {}, tags {}",
            l.len(),
            l.iter().filter(|x| !x.id.is_empty()).count(),
            l.iter().filter(|x| !x.date.is_empty()).count(),
            l.iter().filter(|x| !x.what.is_empty()).count(),
            l.iter().filter(|x| !x.why.is_empty()).count(),
            l.iter().filter(|x| !x.resolution.is_empty()).count(),
            l.iter().filter(|x| !x.tags.is_empty()).count(),
        );
        let t = &k.todos;
        println!(
            "todos {}: table {}, sections {}, closed {}, open {}, with id {}, with file {}, with refs {}",
            t.len(),
            t.iter().filter(|x| x.source == "table").count(),
            t.iter().filter(|x| x.source == "section").count(),
            t.iter().filter(|x| x.closed).count(),
            t.iter().filter(|x| !x.closed).count(),
            t.iter().filter(|x| !x.id.is_empty()).count(),
            t.iter().filter(|x| !x.file.is_empty()).count(),
            t.iter().filter(|x| !x.refs.is_empty()).count(),
        );
        if let Some(left) = &k.left_off {
            println!(
                "handoff {} ({} found); slots: worked_on {}, decisions {}, blockers {}, current {}, next {}",
                left.path,
                left.sources.len(),
                !left.worked_on.is_empty(),
                !left.decisions.is_empty(),
                left.blockers.len(),
                !left.current.is_empty(),
                left.next.len()
            );
        }
        let mut ask_kinds: HashMap<&str, usize> = HashMap::new();
        for a in &k.asks {
            *ask_kinds.entry(a.source.kind).or_default() += 1;
        }
        println!("asks {} by source {ask_kinds:?}", k.asks.len());
        let tidy: Vec<(&str, usize)> = k.tidy.iter().map(|t| (t.kind, t.refs.len())).collect();
        println!("tidy (kind, refs) {tidy:?}");
        println!(
            "conventions {} (with id {}), sessions {}, questions {}, guidance {}",
            k.conventions.len(),
            k.conventions.iter().filter(|c| !c.id.is_empty()).count(),
            k.sessions.len(),
            k.questions.len(),
            k.guidance.len()
        );
        println!("counts {}", serde_json::to_string(&k.counts).unwrap());
        println!("warnings {}", k.warnings.len());
        for w in &k.warnings {
            // Paths and counts only.
            println!("  warning: {}", w.chars().take(160).collect::<String>());
        }
    }
}

/// One test per shape real projects write, and the reference tree end to
/// end.
#[cfg(test)]
mod shapes;
