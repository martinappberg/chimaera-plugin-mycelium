//! Scanners over an entry's markdown, one line at a time: the ids it cites
//! (`refs`), the files, jobs and commits it names (`cites`), the state
//! markers and amends its author wrote, and the sentences that put something
//! to the user (`asks`). Plus the small text helpers they share.
//!
//! Everything here reports what the text SAYS. Nothing infers, grades or
//! second-guesses: a finding is superseded because its heading says
//! `SUPERSEDED by F-177`, never because a newer one exists. Every scanner is
//! a linear pass over its input with bounded output, so ~2 MB of markdown
//! stays well inside the plugin's time and memory budget.

use serde::Serialize;

/// Ids kept per entry.
pub(crate) const MAX_REFS: usize = 50;
/// Files, jobs and commits kept per entry.
pub(crate) const MAX_CITES: usize = 30;
/// Amends kept per entry.
pub(crate) const MAX_AMENDS: usize = 20;
/// An ask's sentence, "…" included, in chars.
pub(crate) const MAX_ASK_CHARS: usize = 300;

/// An id an entry cites: `kind` ∈ finding | decision | convention |
/// learning | todo, `id` as written (`F-171`, `T-GroupTiers`).
#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub(crate) struct Ref {
    pub(crate) kind: &'static str,
    pub(crate) id: String,
}

/// A file, job or commit an entry names: `kind` ∈ script | data | figure |
/// doc | path | job | commit.
#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub(crate) struct Cite {
    pub(crate) kind: &'static str,
    pub(crate) text: String,
}

/// What the text says became of an entry: superseded | corrected |
/// retracted | suspect | resolved, and the id that did it when known.
#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub(crate) struct State {
    pub(crate) kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) by: Option<String>,
}

/// What an entry says it does to another: corrects | supersedes | retracts.
#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub(crate) struct Amend {
    pub(crate) kind: &'static str,
    pub(crate) id: String,
}

// ---------------------------------------------------------------------------
// Small text helpers
// ---------------------------------------------------------------------------

pub(crate) fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `[text](target)` → `text` (images too), plus the first link's target.
pub(crate) fn strip_links(s: &str) -> (String, Option<String>) {
    let mut out = String::with_capacity(s.len());
    let mut target = None;
    let mut rest = s;
    while let Some(open) = rest.find('[') {
        let Some(close) = rest[open..].find("](").map(|at| open + at) else {
            break;
        };
        let Some(end) = rest[close + 2..].find(')').map(|at| close + 2 + at) else {
            break;
        };
        let before = &rest[..open];
        out.push_str(before.strip_suffix('!').unwrap_or(before));
        out.push_str(&rest[open + 1..close]);
        if target.is_none() {
            // `(path "title")`: the path is the first token.
            let link = rest[close + 2..end].split_whitespace().next().unwrap_or("");
            target = Some(link.trim_matches(['<', '>']).to_owned());
        }
        rest = &rest[end + 1..];
    }
    out.push_str(rest);
    (out, target)
}

/// Markdown stripped to its words: links unwrapped, emphasis, code and
/// strike marks dropped, a blockquote's `>` dropped, whitespace collapsed.
/// Everything else stays as written.
pub(crate) fn plain(s: &str) -> String {
    let (text, _) = strip_links(s);
    let text = text.trim_start().trim_start_matches('>');
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '*' | '`' => {}
            '~' if chars.peek() == Some(&'~') => {
                chars.next();
            }
            _ => out.push(c),
        }
    }
    collapse_ws(&out)
}

/// At most `max` chars, "…" included, cut on a char boundary.
pub(crate) fn cap_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_owned();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    let trimmed = out.trim_end().len();
    out.truncate(trimmed);
    out.push('…');
    out
}

fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Whether the char before byte `at` of `s` ends a word (or there is none).
fn starts_word(s: &str, at: usize) -> bool {
    s[..at]
        .chars()
        .next_back()
        .is_none_or(|c| !(c.is_alphanumeric() || c == '_'))
}

// ---------------------------------------------------------------------------
// Dates
// ---------------------------------------------------------------------------

pub(crate) fn is_iso_date(b: &[u8]) -> bool {
    b.len() == 10
        && b.iter().enumerate().all(|(i, c)| match i {
            4 | 7 => *c == b'-',
            _ => c.is_ascii_digit(),
        })
}

/// Days since 1970-01-01 of a `YYYY-MM-DD` prefix (Howard Hinnant's
/// days_from_civil), for "within N days" windows.
pub(crate) fn day_number(date: &str) -> Option<i64> {
    let head = date.as_bytes().get(..10)?;
    if !is_iso_date(head) {
        return None;
    }
    let y: i64 = date[..4].parse().ok()?;
    let m: i64 = date[5..7].parse().ok()?;
    let d: i64 = date[8..10].parse().ok()?;
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era * 146_097 + doe - 719_468)
}

/// The UTC calendar date of an epoch-ms instant (civil_from_days).
pub(crate) fn date_of_ms(ms: u64) -> String {
    let z = (ms / 86_400_000) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

/// A heading's trailing `(YYYY-MM-DD)` — or `(YYYY-MM-DD, …)` / `(YYYY-MM-DD
/// 23:54 PDT)`, a parenthesis that opens with the date — as `(the text
/// before it, the date)`.
pub(crate) fn trailing_date(text: &str) -> Option<(&str, &str)> {
    let t = text.trim_end().trim_end_matches('*').trim_end();
    let body = t.strip_suffix(')')?;
    let open = body.rfind('(')?;
    let inside = &body[open + 1..];
    let date = inside.get(..10).filter(|d| is_iso_date(d.as_bytes()))?;
    if !(inside.len() == 10 || inside[10..].starts_with([' ', ',', ';'])) {
        return None;
    }
    Some((t[..open].trim_end(), date))
}

/// The first `(YYYY-MM-DD…)` anywhere in `text`: a follow-up heading's date
/// sits after its word (`CORRECTION (2026-07-23): …`).
pub(crate) fn paren_date(text: &str) -> Option<&str> {
    let mut rest = text;
    while let Some(open) = rest.find('(') {
        let inside = &rest[open + 1..];
        if let Some(date) = inside.get(..10).filter(|d| is_iso_date(d.as_bytes())) {
            return Some(date);
        }
        rest = inside;
    }
    None
}

// ---------------------------------------------------------------------------
// Ids
// ---------------------------------------------------------------------------

/// The entry id starting at byte `at` of `s`, as `(kind, id)`: `F-`, `D-`,
/// `L-` + 1–4 digits, `C-` + 1–3 digits, `T-` + a name with a letter
/// (`T-GroupTiers`, `T-07Linkage`). The char before `at` must not be part
/// of a word ("COVID-19" holds no `D-19`); the id must end a word too.
pub(crate) fn id_at(s: &str, at: usize) -> Option<(&'static str, &str)> {
    let b = s.as_bytes();
    let (kind, max) = match b.get(at)? {
        b'F' => ("finding", 4),
        b'D' => ("decision", 4),
        b'C' => ("convention", 3),
        b'L' => ("learning", 4),
        b'T' => ("todo", 0),
        _ => return None,
    };
    if b.get(at + 1) != Some(&b'-') || !starts_word(s, at) {
        return None;
    }
    let start = at + 2;
    let digits = b[start..].iter().take_while(|c| c.is_ascii_digit()).count();
    let end = if kind == "todo" {
        if !b.get(start + digits).is_some_and(u8::is_ascii_alphabetic) {
            return None;
        }
        let name = b[start + digits..]
            .iter()
            .take_while(|c| c.is_ascii_alphanumeric())
            .count();
        let end = start + digits + name;
        // A hyphenated word ("T-cell-specific") is prose, not a to-do.
        if b.get(end) == Some(&b'-') && b.get(end + 1).is_some_and(u8::is_ascii_alphanumeric) {
            return None;
        }
        end
    } else {
        if digits == 0 || digits > max {
            return None;
        }
        start + digits
    };
    if b.get(end).copied().is_some_and(is_word_byte) {
        return None;
    }
    Some((kind, &s[at..end]))
}

/// Every id in `line`, in order of first appearance, `own` excluded,
/// appended to `refs` up to [`MAX_REFS`].
pub(crate) fn scan_refs(line: &str, own: &str, refs: &mut Vec<Ref>) {
    if refs.len() >= MAX_REFS || !line.contains('-') {
        return;
    }
    let b = line.as_bytes();
    for at in 0..b.len() {
        if !matches!(b[at], b'F' | b'D' | b'C' | b'L' | b'T') || b.get(at + 1) != Some(&b'-') {
            continue;
        }
        let Some((kind, id)) = id_at(line, at) else {
            continue;
        };
        if id == own || refs.iter().any(|r| r.id == id) {
            continue;
        }
        refs.push(Ref {
            kind,
            id: id.to_owned(),
        });
        if refs.len() >= MAX_REFS {
            return;
        }
    }
}

// ---------------------------------------------------------------------------
// Cites
// ---------------------------------------------------------------------------

const SCRIPT: &[&str] = &[
    ".py", ".r", ".sh", ".ipynb", ".smk", ".nf", ".jl", ".pl", ".rs", ".ts", ".js", ".sql",
];
const DATA: &[&str] = &[
    ".h5ad", ".h5", ".tsv", ".csv", ".txt.gz", ".parquet", ".yaml", ".yml", ".json", ".bed",
    ".bam", ".vcf", ".gz", ".rds", ".loom", ".zarr", ".mtx", ".npz", ".pkl", ".xlsx",
];
const FIGURE: &[&str] = &[".png", ".pdf", ".svg", ".jpg", ".jpeg"];
const DOC: &[&str] = &[".md"];

fn push_cite(cites: &mut Vec<Cite>, kind: &'static str, text: &str) {
    if cites.len() >= MAX_CITES || cites.iter().any(|c| c.kind == kind && c.text == text) {
        return;
    }
    cites.push(Cite {
        kind,
        text: text.to_owned(),
    });
}

/// A path-like token's kind by extension; `in_code` (a code span or a link
/// target) also takes an extension-less path with a `/`.
fn classify(token: &str, in_code: bool) -> Option<(&'static str, &str)> {
    let t = token.trim_matches(|c: char| {
        matches!(
            c,
            '(' | ')'
                | '['
                | ']'
                | '"'
                | '\''
                | '<'
                | '>'
                | ','
                | ';'
                | '!'
                | '?'
                | '“'
                | '”'
                | '‘'
                | '’'
        )
    });
    // Emphasis wraps a path symmetrically (`**x.py**`); any other `*` is part
    // of a glob (`*_R1_001.fastq.gz`, `sample_*.h5ad`), which names no one file.
    let lead = t.bytes().take_while(|&b| b == b'*').count();
    let tail = t.bytes().rev().take_while(|&b| b == b'*').count();
    let t = t.trim_matches('*');
    if lead != tail || t.contains('*') {
        return None;
    }
    // `--in=path/x.tsv`: the value.
    let t = t.rsplit_once('=').map_or(t, |(_, v)| v);
    let t = t.trim_end_matches(['.', ':', ',', ';']);
    // `x.py:52`, `x.py:52-60`: the file.
    let t = match t.rsplit_once(':') {
        Some((file, line))
            if !file.is_empty()
                && !line.is_empty()
                && line.bytes().all(|b| b.is_ascii_digit() || b == b'-') =>
        {
            file
        }
        _ => t,
    };
    if t.len() < 3 || t.len() > 300 || t.contains("://") || t.starts_with('-') {
        return None;
    }
    if !t.bytes().any(|b| b.is_ascii_alphanumeric()) {
        return None;
    }
    let bare = t.trim_end_matches('/');
    let lower = bare.to_ascii_lowercase();
    let name = lower.rsplit('/').next().unwrap_or(&lower);
    if !in_code
        && !t.chars().all(|c| {
            c.is_alphanumeric()
                || matches!(
                    c,
                    '_' | '-' | '.' | '/' | '~' | '{' | '}' | ',' | '+' | '$' | '@' | '%' | '#'
                )
        })
    {
        return None;
    }
    let by_ext = [
        ("script", SCRIPT),
        ("data", DATA),
        ("figure", FIGURE),
        ("doc", DOC),
    ]
    .into_iter()
    .find(|(_, exts)| {
        exts.iter()
            .any(|ext| name.len() > ext.len() && name.ends_with(ext))
    });
    if let Some((kind, _)) = by_ext {
        return Some((kind, t));
    }
    (in_code && bare.contains('/')).then_some(("path", t))
}

/// The words of `s` with their byte offsets: runs of ASCII alphanumerics.
fn words(s: &str) -> impl Iterator<Item = (usize, &str)> {
    let b = s.as_bytes();
    let mut i = 0;
    std::iter::from_fn(move || {
        while i < b.len() && !b[i].is_ascii_alphanumeric() {
            i += 1;
        }
        if i >= b.len() {
            return None;
        }
        let start = i;
        while i < b.len() && b[i].is_ascii_alphanumeric() {
            i += 1;
        }
        Some((start, &s[start..i]))
    })
}

fn is_commit(w: &str) -> bool {
    (7..=40).contains(&w.len())
        && w.bytes().all(|b| b.is_ascii_hexdigit())
        && w.bytes().any(|b| b.is_ascii_digit())
        && w.bytes().any(|b| b.is_ascii_alphabetic())
}

/// Slurm job ids (5–10 digits after `job`/`jobs`/`JobID`, a list allowed)
/// and commits (7–40 hex with a digit and a letter after `commit`/`sha`/`@`).
fn scan_jobs_and_commits(line: &str, cites: &mut Vec<Cite>) {
    let all: Vec<(usize, &str)> = words(line).collect();
    let mut k = 0;
    while k < all.len() {
        let (at, word) = all[k];
        let lower = word.to_ascii_lowercase();
        let job = matches!(lower.as_str(), "job" | "jobs" | "jobid" | "jobids");
        let commit = matches!(lower.as_str(), "commit" | "commits" | "sha");
        k += 1;
        if !(job || commit) {
            // `@1f2e3d4`
            if at > 0 && line.as_bytes()[at - 1] == b'@' && is_commit(word) {
                let before = &line[..at - 1];
                if before
                    .chars()
                    .next_back()
                    .is_none_or(|c| !c.is_alphanumeric())
                {
                    push_cite(cites, "commit", word);
                }
            }
            continue;
        }
        while let Some(&(_, next)) = all.get(k) {
            let next_lower = next.to_ascii_lowercase();
            if matches!(next_lower.as_str(), "and" | "id" | "ids") {
                k += 1;
                continue;
            }
            let hit = if job {
                (5..=10).contains(&next.len()) && next.bytes().all(|b| b.is_ascii_digit())
            } else {
                is_commit(next)
            };
            if !hit {
                break;
            }
            push_cite(cites, if job { "job" } else { "commit" }, next);
            k += 1;
        }
    }
}

/// Files, jobs and commits `line` names, appended to `cites` up to
/// [`MAX_CITES`]: code spans and link targets by extension or a `/`, plain
/// words by extension only.
pub(crate) fn scan_cites(line: &str, cites: &mut Vec<Cite>) {
    if cites.len() >= MAX_CITES {
        return;
    }
    if line.contains("job")
        || line.contains("Job")
        || line.contains("JOB")
        || line.contains("ommit")
        || line.contains("sha")
        || line.contains("SHA")
        || line.contains('@')
    {
        scan_jobs_and_commits(&line.replace('`', " "), cites);
    }
    let mut code = false;
    for (n, part) in line.split('`').enumerate() {
        if n > 0 {
            code = !code;
        }
        if code {
            for token in part.split_whitespace() {
                if let Some((kind, text)) = classify(token, true) {
                    push_cite(cites, kind, text);
                }
            }
            continue;
        }
        // Link targets are paths as written; their text is prose.
        let mut rest = part;
        let mut prose = String::new();
        while let Some(open) = rest.find("](") {
            let Some(end) = rest[open + 2..].find(')').map(|at| open + 2 + at) else {
                break;
            };
            prose.push_str(&rest[..open]);
            let target = rest[open + 2..end].split_whitespace().next().unwrap_or("");
            if let Some((kind, text)) = classify(target.split('#').next().unwrap_or(""), true) {
                push_cite(cites, kind, text);
            }
            rest = &rest[end + 1..];
        }
        prose.push_str(rest);
        for token in prose.split_whitespace() {
            if token.contains('.') {
                if let Some((kind, text)) = classify(token, false) {
                    push_cite(cites, kind, text);
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// State markers and amends
// ---------------------------------------------------------------------------

/// Words before `SUPERSEDED` that make it partial or negated: "⛔ SCOPE
/// CLAUSE SUPERSEDED BY D-116" says a clause went, not the entry.
const PARTIAL: &[&str] = &[
    "clause", "part", "section", "half", "premise", "not", "never", "longer",
];

/// Words before a marker that negate it: "no longer SUSPECT", "not
/// RETRACTED".
const NEGATING: &[&str] = &["not", "no", "never", "longer"];

/// Markers are read from a line's first bytes, at most a few times each: a
/// heading or a Status is short, and one pathological line must stay
/// linear.
const MARKER_SCAN_BYTES: usize = 4096;
const MARKER_MATCHES: usize = 8;

/// `line` cut to [`MARKER_SCAN_BYTES`] on a char boundary.
fn marker_head(line: &str) -> &str {
    if line.len() <= MARKER_SCAN_BYTES {
        return line;
    }
    let mut end = MARKER_SCAN_BYTES;
    while !line.is_char_boundary(end) {
        end -= 1;
    }
    &line[..end]
}

/// The word before byte `at`, lowercased ("" when none).
fn word_before(s: &str, at: usize) -> String {
    let head = s[..at].trim_end_matches(|c: char| c.is_whitespace() || c == '*');
    let start = head
        .char_indices()
        .rev()
        .find(|(_, c)| !c.is_alphanumeric())
        .map_or(0, |(i, c)| i + c.len_utf8());
    head[start..].to_lowercase()
}

/// The id after `by` (or `BY`) at byte `at` of `s`, if the text says so.
fn by_id(s: &str, at: usize) -> Option<String> {
    let rest = &s[at..];
    let rest = rest.trim_start_matches(|c: char| c.is_whitespace() || c == '*' || c == ':');
    let rest = rest
        .strip_prefix("by")
        .or_else(|| rest.strip_prefix("BY"))
        .or_else(|| rest.strip_prefix("By"))?;
    let rest_at = s.len() - rest.len();
    let skip = rest
        .find(|c: char| !(c.is_whitespace() || c == '*' || c == ':' || c == '`'))
        .unwrap_or(rest.len());
    id_at(s, rest_at + skip)
        .filter(|(kind, _)| *kind != "todo")
        .map(|(_, id)| id.to_owned())
}

/// Whole-word, case-sensitive occurrences of `word` in `s`.
fn find_word<'a>(s: &'a str, word: &'a str) -> impl Iterator<Item = usize> + 'a {
    s.match_indices(word).map(|(at, _)| at).filter(move |&at| {
        starts_word(s, at)
            && !s
                .as_bytes()
                .get(at + word.len())
                .copied()
                .is_some_and(|b| b.is_ascii_alphabetic())
    })
}

/// Whether another entry's id comes earlier in the sentence that byte `at`
/// is in: "D-40, which relied on this, is superseded by D-41" says what
/// became of D-109, not of the entry that wrote it.
fn about_another(line: &str, at: usize, own: &str) -> bool {
    let start = line[..at].rfind(['.', ';', '!', '?']).map_or(0, |i| i + 1);
    let mut refs = Vec::new();
    scan_refs(&line[start..at], own, &mut refs);
    refs.iter().any(|r| r.kind != "todo")
}

/// `superseded` from `⛔ … SUPERSEDED [BY <id>]`, or `superseded by <id>`
/// in a sentence about this entry.
fn superseded_in(line: &str, own: &str) -> Option<State> {
    let lower = line.to_ascii_lowercase();
    for (at, _) in lower.match_indices("superseded").take(MARKER_MATCHES) {
        if !starts_word(&lower, at) || PARTIAL.contains(&word_before(line, at).as_str()) {
            continue;
        }
        let after = at + "superseded".len();
        let by = by_id(line, after);
        let stop_sign = line[..at]
            .rfind('⛔')
            .is_some_and(|sign| !line[sign..at].contains(['.', ';', '(']));
        let says_by = by.is_some()
            || line[after..]
                .trim_start_matches(|c: char| c.is_whitespace() || c == '*')
                .get(..3)
                .is_some_and(|w| w.eq_ignore_ascii_case("by "));
        if stop_sign || (says_by && !about_another(line, at, own)) {
            return Some(State {
                kind: "superseded",
                by,
            });
        }
    }
    None
}

/// `⚠️ SUSPECT` — the marker, not the word ("the earlier SUSPECT flag was
/// cleared", "no longer SUSPECT").
fn suspect_in(line: &str) -> bool {
    find_word(line, "SUSPECT").take(MARKER_MATCHES).any(|at| {
        line[..at]
            .rfind('⚠')
            .is_some_and(|sign| !line[sign..at].contains(['.', ';']))
            && !NEGATING.contains(&word_before(line, at).as_str())
    })
}

/// An id written right before byte `at` ("F-037 RETRACTED"), if any.
fn id_before(s: &str, at: usize) -> Option<&str> {
    let head = s[..at].trim_end_matches(|c: char| c.is_whitespace() || c == '*' || c == ':');
    let start = head
        .char_indices()
        .rev()
        .find(|(_, c)| !(c.is_ascii_alphanumeric() || *c == '-'))
        .map_or(0, |(i, c)| i + c.len_utf8());
    let (_, id) = id_at(s, start)?;
    (start + id.len() == head.len()).then_some(id)
}

/// Own `retracted` from `RETRACTED` in a heading or a Status — unless another
/// entry's id stands right before it ("F-037 RETRACTED": that one is).
fn retracted_in(line: &str, own: &str) -> Option<State> {
    find_word(line, "RETRACTED")
        .take(MARKER_MATCHES)
        .find_map(|at| {
            let named = id_before(line, at);
            // "not RETRACTED", "was RETRACTED in error; reinstated".
            let sentence = &line[at..];
            let sentence = &sentence[..sentence.find(['.', ';']).unwrap_or(sentence.len())];
            let lower = sentence.to_ascii_lowercase();
            let undone = NEGATING.contains(&word_before(line, at).as_str())
                || lower.contains("in error")
                || lower.contains("reinstated");
            let own_sentence =
                named.is_none_or(|id| id == own) && !undone && !about_another(line, at, own);
            own_sentence.then(|| State {
                kind: "retracted",
                by: by_id(line, at + "RETRACTED".len()),
            })
        })
}

/// The state an entry's own text declares: `heading` and `status` may say
/// RETRACTED; the heading and `first` lines may say SUPERSEDED or SUSPECT.
/// Retracted outranks superseded, which outranks suspect.
pub(crate) fn own_state(heading: &str, status: &str, first: &[&str], own: &str) -> Option<State> {
    let (heading, status) = (marker_head(heading), marker_head(status));
    if let Some(state) = retracted_in(heading, own).or_else(|| retracted_in(status, own)) {
        return Some(state);
    }
    let lines = || {
        std::iter::once(heading)
            .chain(std::iter::once(status))
            .chain(first.iter().map(|l| marker_head(l)))
    };
    if let Some(state) = lines().find_map(|l| superseded_in(l, own)) {
        return Some(state);
    }
    lines().any(suspect_in).then_some(State {
        kind: "suspect",
        by: None,
    })
}

fn push_amend(amends: &mut Vec<Amend>, kind: &'static str, id: &str, own: &str) {
    if id == own
        || amends.len() >= MAX_AMENDS
        || amends.iter().any(|a| a.kind == kind && a.id == id)
    {
        return;
    }
    amends.push(Amend {
        kind,
        id: id.to_owned(),
    });
}

/// `corrects F-171`, `This CORRECTS F-171`, `supersedes D-121`, `retracts
/// F-037` (any case, `**` and `:` tolerated, a list of ids joined by `/`,
/// `,`, `and`, `&`) — but not `supersedes D-91's threshold`: a
/// possessive names a part of the entry, not the entry.
pub(crate) fn scan_amends(line: &str, own: &str, amends: &mut Vec<Amend>) {
    let lower = line.to_ascii_lowercase();
    for (verb, kind) in [
        ("corrects", "corrects"),
        ("supersedes", "supersedes"),
        ("retracts", "retracts"),
    ] {
        for (at, _) in lower.match_indices(verb) {
            if !starts_word(&lower, at) {
                continue;
            }
            let mut i = at + verb.len();
            if lower
                .as_bytes()
                .get(i)
                .is_some_and(u8::is_ascii_alphanumeric)
            {
                continue;
            }
            loop {
                let skip = line[i..]
                    .find(|c: char| !(c.is_whitespace() || matches!(c, '*' | ':' | '`')))
                    .unwrap_or(line.len() - i);
                i += skip;
                let Some((id_kind, id)) = id_at(line, i) else {
                    break;
                };
                if id_kind == "todo" {
                    break;
                }
                let end = i + id.len();
                let possessive = line[end..].starts_with("'s") || line[end..].starts_with("’s");
                if !possessive {
                    push_amend(amends, kind, id, own);
                }
                // Another id in the list?
                let rest = &line[end..];
                let sep =
                    rest.trim_start_matches(|c: char| c.is_whitespace() || c == '*' || c == '`');
                let next = ["/", ",", "&", "and "]
                    .iter()
                    .find_map(|s| sep.strip_prefix(s));
                match next {
                    Some(after) if !possessive => i = line.len() - after.len(),
                    _ => break,
                }
            }
        }
    }
}

/// `F-037 RETRACTED` in a heading or an entry's first lines: this entry
/// retracts F-037.
pub(crate) fn scan_retracted_ids(line: &str, own: &str, amends: &mut Vec<Amend>) {
    let line = marker_head(line);
    for at in find_word(line, "RETRACTED").take(MARKER_MATCHES) {
        if let Some(id) = id_before(line, at).filter(|id| *id != own) {
            if !id.starts_with("T-") {
                push_amend(amends, "retracts", id, own);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Asks
// ---------------------------------------------------------------------------

/// What agents write when they put something to the user (lowercase; `’`
/// reads as `'`).
const ASK_PHRASES: &[&str] = &[
    "put to the user",
    "for the user",
    "user decision",
    "user's call",
    "parked user decision",
    "not yet decided",
    "awaiting the user",
];

fn has_ask(lower: &str) -> bool {
    ASK_PHRASES.iter().any(|p| lower.contains(p))
}

/// Byte ranges of `s`'s sentences: a `.`/`?`/`!` (closing `*`, `)`, quotes
/// allowed) then whitespace then a capital, digit, `*`, `(`, `` ` `` or `[`.
fn sentences(s: &str) -> Vec<(usize, usize)> {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut start = 0;
    let mut i = 0;
    while i < b.len() {
        if matches!(b[i], b'.' | b'?' | b'!') {
            let mut j = i + 1;
            while j < b.len() && matches!(b[j], b'*' | b')' | b'"' | b'\'' | b'_') {
                j += 1;
            }
            let ws = b[j..]
                .iter()
                .take_while(|c| c.is_ascii_whitespace())
                .count();
            let k = j + ws;
            if ws > 0
                && s[k..].chars().next().is_some_and(|c| {
                    c.is_uppercase() || c.is_ascii_digit() || matches!(c, '*' | '(' | '`' | '[')
                })
            {
                out.push((start, j));
                start = k;
                i = k;
                continue;
            }
        }
        i += 1;
    }
    if start < b.len() {
        out.push((start, b.len()));
    }
    out
}

/// The sentences of a paragraph that put something to the user, markdown
/// stripped and capped at [`MAX_ASK_CHARS`].
pub(crate) fn ask_sentences(paragraph: &str) -> Vec<String> {
    let text = paragraph.replace('’', "'");
    if !has_ask(&text.to_ascii_lowercase()) {
        return Vec::new();
    }
    // `**Tags**: … · **Status**: the fix is the user's call`: one field.
    text.split('·')
        .flat_map(|part| {
            let lower = part.to_ascii_lowercase();
            sentences(part)
                .into_iter()
                .filter(move |&(a, z)| has_ask(&lower[a..z]))
                .map(move |(a, z)| cap_chars(&plain(&part[a..z]), MAX_ASK_CHARS))
        })
        .filter(|s| !s.is_empty())
        .collect()
}

/// A paragraph's first sentence, markdown stripped.
pub(crate) fn first_sentence(text: &str) -> String {
    let (a, z) = sentences(text).first().copied().unwrap_or((0, text.len()));
    plain(&text[a..z])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(line: &str, own: &str) -> Vec<String> {
        let mut refs = Vec::new();
        scan_refs(line, own, &mut refs);
        refs.into_iter()
            .map(|r| format!("{}:{}", r.kind, r.id))
            .collect()
    }

    #[test]
    fn ids_are_words_of_their_own() {
        assert_eq!(
            ids(
                "F-020/F-021 addendum; see D-48/D-49, C-12 and L-40 (T-GroupTiers, T-07Linkage).",
                "F-020"
            ),
            [
                "finding:F-021",
                "decision:D-48",
                "decision:D-49",
                "convention:C-12",
                "learning:L-40",
                "todo:T-GroupTiers",
                "todo:T-07Linkage"
            ]
        );
        // Not ids: inside a word, too many digits, a hyphenated word,
        // lowercase, a bare letter.
        assert!(ids(
            "COVID-19, HLA-DRB1, F-12345, T-cell-specific, f-027, L — note, CD-1, T-2",
            ""
        )
        .is_empty());
        // Repeats count once, in order of first appearance.
        assert_eq!(
            ids("D-152..D-156, D-152 again", ""),
            ["decision:D-152", "decision:D-156"]
        );
    }

    fn cites(line: &str) -> Vec<String> {
        let mut out = Vec::new();
        scan_cites(line, &mut out);
        out.into_iter()
            .map(|c| format!("{}:{}", c.kind, c.text))
            .collect()
    }

    #[test]
    fn cites_are_files_jobs_and_commits() {
        assert_eq!(
            cites("Regenerate from `~/r03_timing.py` over 03_runs/v2/x.tsv (job 51973032)."),
            [
                "job:51973032",
                "script:~/r03_timing.py",
                "data:03_runs/v2/x.tsv"
            ]
        );
        assert_eq!(
            cites("jobs 51000001/51000002 and 51000003; commit 1f2e3d4, sha 0a1b2c3d4"),
            [
                "job:51000001",
                "job:51000002",
                "job:51000003",
                "commit:1f2e3d4",
                "commit:0a1b2c3d4"
            ]
        );
        assert_eq!(
            cites("`raw/sample-a/fragments/` and `corpus_check.py:52`, see [notes](notes/plan.md) and fig.png"),
            [
                "path:raw/sample-a/fragments/",
                "script:corpus_check.py",
                "doc:notes/plan.md",
                "figure:fig.png"
            ]
        );
        // Not cites: a glob, a flag, a URL, prose with a period, a 4-digit job.
        assert_eq!(
            cites(
                "`*_R1_001.fastq.gz` `--force` https://example.org/x.py e.g. v1.2 job 1234 commit abcdefg"
            ),
            Vec::<String>::new()
        );
        assert_eq!(cites("pinned @1a2b3c4d"), ["commit:1a2b3c4d"]);
        assert!(cites("mail me@1a2b3c4d").is_empty());
    }

    fn state(heading: &str, status: &str, first: &[&str]) -> Option<(String, Option<String>)> {
        own_state(heading, status, first, "F-041").map(|s| (s.kind.to_owned(), s.by))
    }

    #[test]
    fn state_comes_only_from_what_the_text_says() {
        let some = |kind: &str, by: Option<&str>| Some((kind.to_owned(), by.map(str::to_owned)));
        assert_eq!(
            state(
                "D-104 — keep the old index — ⛔ SUPERSEDED BY D-110 (2026-09-02)",
                "",
                &[]
            ),
            some("superseded", Some("D-110"))
        );
        assert_eq!(
            state(
                "F-041 (SUPERSEDED by F-177 — the inputs are converted): x",
                "",
                &[]
            ),
            some("superseded", Some("F-177"))
        );
        assert_eq!(
            state(
                "A clause",
                "",
                &["> ⚠️ **SUSPECT as of 2026-09-02 — do not cite.**"]
            ),
            some("suspect", None)
        );
        assert_eq!(
            state("x", "RETRACTED — see below", &[]),
            some("retracted", None)
        );
        // Another entry's id before RETRACTED: that one is retracted.
        assert_eq!(
            state("F-041 — F-037 RETRACTED: the input was truncated", "", &[]),
            None
        );
        // A clause, a mention, a later body line: not the entry's state.
        assert_eq!(
            state("D-89 — ⛔ SCOPE CLAUSE SUPERSEDED BY D-116", "", &[]),
            None
        );
        assert_eq!(state("The superseded pipeline leaked", "", &[]), None);
        assert_eq!(state("SUSPECTED doublets", "", &[]), None);
        // A sentence about another entry says what became of that one.
        assert_eq!(
            state(
                "x",
                "",
                &["Re-run first. D-40, which relied on this, is superseded by D-41."]
            ),
            None
        );
        assert_eq!(state("x", "", &["F-185's labels are SUSPECT."]), None);
        // Negated or undone markers are no state.
        for (heading, status, first) in [
            ("x", "supported — no longer SUSPECT after the rerun", ""),
            ("x", "", "the earlier SUSPECT flag was cleared"),
            ("x", "robust (was RETRACTED in error; reinstated)", ""),
            ("F-041 — groups hold, not RETRACTED", "", ""),
            ("x", "", "> ⚠️ no longer SUSPECT"),
            ("x", "no longer superseded by D-12", ""),
        ] {
            assert_eq!(
                state(heading, status, &[first]),
                None,
                "{heading} | {status} | {first}"
            );
        }
        assert_eq!(
            state(
                "x",
                "",
                &["> ⚠️ **SUSPECT as of 2026-09-02** — F-185 made the labels."]
            ),
            some("suspect", None)
        );
    }

    fn amends(line: &str) -> Vec<String> {
        let mut out = Vec::new();
        scan_amends(line, "F-178", &mut out);
        scan_retracted_ids(line, "F-178", &mut out);
        out.into_iter()
            .map(|a| format!("{}:{}", a.kind, a.id))
            .collect()
    }

    #[test]
    fn amends_name_whole_entries() {
        assert_eq!(
            amends("**Status:** established by an independent rerun. **This CORRECTS F-171.**"),
            ["corrects:F-171"]
        );
        assert_eq!(
            amends("This supersedes F-184 / D-109 by reproducing them; retracts F-037."),
            ["supersedes:F-184", "supersedes:D-109", "retracts:F-037"]
        );
        assert_eq!(
            amends("F-037 RETRACTED: the input was truncated"),
            ["retracts:F-037"]
        );
        // A possessive names a part; the entry's own id is never an amend.
        assert!(amends("supersedes D-91's threshold; corrects F-178").is_empty());
        assert!(amends("which correctsF-12, a supersedes-D-1 typo").is_empty());
    }

    #[test]
    fn asks_are_sentences_that_put_something_to_the_user() {
        assert_eq!(
            ask_sentences(
                "**Consequence.** Put to the user with the stage 08 thresholds (F-227): one rerun \
                 from `stage07`. Nothing else changes."
            ),
            ["Put to the user with the stage 08 thresholds (F-227): one rerun from stage07."]
        );
        assert_eq!(
            ask_sentences("The fix is the user’s call. It costs a day."),
            ["The fix is the user's call."]
        );
        assert!(ask_sentences("The user ran it. Done.").is_empty());
        assert_eq!(
            ask_sentences(
                "**Tags**: a, b · **Status**: finding; the fix is the user's call (a rerun)"
            ),
            ["Status: finding; the fix is the user's call (a rerun)"]
        );
        let long = format!("Not yet decided: {}", "word ".repeat(100));
        assert_eq!(ask_sentences(&long)[0].chars().count(), MAX_ASK_CHARS);
    }

    #[test]
    fn dates_count_days() {
        assert_eq!(day_number("1970-01-01"), Some(0));
        assert_eq!(day_number("2026-09-28"), Some(20_724));
        assert_eq!(
            day_number("2026-09-28").unwrap() - day_number("2026-09-14").unwrap(),
            14
        );
        assert_eq!(day_number("2026-13-01"), None);
        assert_eq!(date_of_ms(20_724 * 86_400_000 + 5), "2026-09-28");
        assert_eq!(date_of_ms(0), "1970-01-01");
        assert_eq!(
            trailing_date("F-228 — words (2026-09-28)"),
            Some(("F-228 — words", "2026-09-28"))
        );
        assert_eq!(
            trailing_date("RESOLVED (2026-07-23 23:54 PDT)"),
            Some(("RESOLVED", "2026-07-23"))
        );
        assert_eq!(trailing_date("gate input (not 2026-09-28)"), None);
        assert_eq!(
            paren_date("CORRECTION (2026-08-31): three"),
            Some("2026-08-31")
        );
    }

    #[test]
    fn plain_text_and_first_sentences() {
        assert_eq!(
            plain("> **Option B** only — see [notes](x.md) and `a_b`, ~~old~~"),
            "Option B only — see notes and a_b, old"
        );
        assert_eq!(
            first_sentence("**Shared display names.** One table. Two."),
            "Shared display names."
        );
        assert_eq!(cap_chars("abcdef", 4), "abc…");
        assert_eq!(cap_chars("abc", 4), "abc");
    }
}

#[cfg(test)]
mod linear {
    use super::*;

    /// One pathological line — a heading, a Status or a first line of
    /// 2 MB of markers — is read in bounded time.
    #[test]
    fn marker_scans_stay_linear_on_one_huge_line() {
        let started = std::time::Instant::now();
        for word in [
            "RETRACTED ",
            "SUSPECT ",
            "superseded ",
            "⚠️ SUSPECT ",
            "F-12 RETRACTED ",
        ] {
            let line = format!("D-1 see D-2 {}", word.repeat(2_000_000 / word.len()));
            let _ = own_state(&line, &line, &[&line, &line, &line], "D-1");
            let mut amends = Vec::new();
            scan_retracted_ids(&line, "D-1", &mut amends);
            scan_amends(&line, "D-1", &mut amends);
        }
        assert!(started.elapsed().as_secs() < 20, "{:?}", started.elapsed());
    }
}
