//! Questions to the LLM about a node of the review: a symbol of the code
//! graph, a group of the Stack view, or a hunk.
//!
//! The prompt is built only from data survol already computed: the code of
//! the node read from git objects, its callers / callees / tests and other
//! links with their `file:line`, the group summary, the hunks, and the
//! project's `.survol/instructions.md`. The answer cites code as
//! `[path:line]`: references are parsed into links and checked against the
//! lines actually given to the model, so that an invented reference is
//! flagged and never followed. Answers are cached by prompt and model.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt::Write as _;
use std::io;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use serde::{Deserialize, Serialize};

use crate::graph::{EdgeKind, Graph, Link, Role, SymIdx, SymbolKind};
use crate::group::Grouping;
use crate::llm::{LlmError, LlmProvider, LlmRequest};
use crate::model::{Diff, LineKind, Side};

/// Version of `prompts/ask.md` and of the context layout: part of the cache key.
pub const ASK_PROMPT_VERSION: u32 = 1;

const TEMPLATE: &str = include_str!("../prompts/ask.md");
/// Context size budget, in characters.
const MAX_CONTEXT: usize = 80_000;
/// Longest symbol body shown.
const MAX_SYMBOL_LINES: u32 = 120;
const MAX_INSTRUCTIONS: usize = 8_000;
const MAX_CALLERS: usize = 12;
const MAX_CALLEES: usize = 15;
const MAX_TESTS: usize = 10;
const MAX_OTHER_LINKS: usize = 20;
const MAX_SYMBOL_HUNKS: usize = 6;
const MAX_GROUP_SYMBOLS: usize = 25;

/// What a question is about. Ids are stable across runs: a symbol id, a
/// group id, a hunk content hash.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum Subject {
    /// [`crate::graph::Symbol::id`], e.g. `src/Owner.java#Owner.addPet/1`.
    Symbol(String),
    /// [`crate::group::Group::id`].
    Group(usize),
    /// [`crate::model::Hunk::content_hash`].
    Hunk(String),
}

impl Subject {
    /// Questions offered before the reviewer types their own.
    pub fn suggestions(&self) -> &'static [&'static str] {
        match self {
            Subject::Symbol(_) => &[
                "What does this component do?",
                "Why does the code go through here?",
                "What depends on it, and what would a change here affect?",
                "How is it tested?",
            ],
            Subject::Group(_) => &[
                "What does this change do, functionally?",
                "How do the pieces of this group fit together?",
                "Which entry points reach this code?",
                "What should I look at first in this group?",
            ],
            Subject::Hunk(_) => &[
                "What does this change do?",
                "Why is this change needed here?",
                "Who is affected by this change?",
            ],
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AskError {
    #[error("{0}")]
    Subject(String),
    #[error(transparent)]
    Llm(#[from] LlmError),
    #[error("the LLM gave an empty answer")]
    Empty,
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// What the prompt is built from.
pub struct Sources<'a> {
    pub diff: &'a Diff,
    pub graph: Option<&'a Graph>,
    pub grouping: Option<&'a Grouping>,
    /// Reads a file at a revision, `(rev, path)`; `None` when it does not exist.
    pub read: &'a dyn Fn(&str, &str) -> Option<String>,
    pub base_sha: &'a str,
    pub head_sha: &'a str,
}

/// Lines given to the model, by file and side: the only valid references.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RefIndex {
    lines: BTreeMap<String, [BTreeSet<u32>; 2]>,
}

fn side_slot(side: Side) -> usize {
    match side {
        Side::Old => 0,
        Side::New => 1,
    }
}

impl RefIndex {
    pub fn add(&mut self, path: &str, side: Side, line: u32) {
        if line > 0 {
            self.lines.entry(path.to_string()).or_default()[side_slot(side)].insert(line);
        }
    }

    pub fn contains(&self, path: &str, side: Side, line: u32) -> bool {
        self.lines
            .get(path)
            .is_some_and(|s| s[side_slot(side)].contains(&line))
    }

    pub fn files(&self) -> impl Iterator<Item = &str> {
        self.lines.keys().map(String::as_str)
    }

    /// The context file `path` designates: exact, or the only file ending with it.
    pub fn resolve(&self, path: &str) -> Option<&str> {
        let path = path.trim_start_matches("./");
        if let Some((k, _)) = self.lines.get_key_value(path) {
            return Some(k);
        }
        let suffix = format!("/{path}");
        let mut hits = self.lines.keys().filter(|k| k.ends_with(&suffix));
        let first = hits.next()?;
        hits.next().is_none().then_some(first.as_str())
    }
}

/// A prompt ready to send, with what it may be answered about.
#[derive(Debug, Clone)]
pub struct Prompt {
    pub subject: Subject,
    /// Short description of the subject, for display.
    pub label: String,
    pub question: String,
    pub text: String,
    pub refs: RefIndex,
}

/// A `[path:line]` reference of an answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodeRef {
    /// Byte range of the reference in [`Answer::text`].
    pub start: usize,
    pub end: usize,
    /// Path as written by the model.
    pub raw_path: String,
    /// Full path of the context file it designates (the raw path if none).
    pub path: String,
    pub line: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_line: Option<u32>,
    pub side: Side,
    /// The line was part of the context. Invalid references are shown as
    /// such and cannot be followed.
    pub valid: bool,
}

/// An answer, as cached and kept in the review's history.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Answer {
    pub subject: Subject,
    pub label: String,
    pub question: String,
    pub text: String,
    pub refs: Vec<CodeRef>,
    pub model: Option<String>,
    pub head_sha: String,
    pub prompt_version: u32,
    /// Cache key, see [`cache_key`].
    pub key: String,
    /// Seconds since the Unix epoch.
    pub asked_at: u64,
    #[serde(default)]
    pub from_cache: bool,
}

impl Answer {
    pub fn unknown_refs(&self) -> usize {
        self.refs.iter().filter(|r| !r.valid).count()
    }
}

// ----- prompt ---------------------------------------------------------------

/// Builds the prompt of `question` about `subject`.
pub fn build_prompt(
    src: &Sources,
    subject: &Subject,
    question: &str,
    instructions: Option<&str>,
    language: &str,
) -> Result<Prompt, AskError> {
    let mut b = Builder {
        src,
        out: String::new(),
        refs: RefIndex::default(),
        files: HashMap::new(),
    };
    let label = match subject {
        Subject::Symbol(id) => {
            let g = src
                .graph
                .ok_or_else(|| AskError::Subject("the code graph is not built".into()))?;
            let s = g
                .by_id(id)
                .ok_or_else(|| AskError::Subject(format!("unknown symbol `{id}`")))?;
            b.symbol(g, s, true);
            b.groups_of(g.hunks_of_symbol(s));
            format!("{} {}", kind_name(g.symbol(s).kind), g.display_name(s))
        }
        Subject::Group(id) => {
            let grouping = src
                .grouping
                .ok_or_else(|| AskError::Subject("the Stack groups are not ready".into()))?;
            let (i, group) = grouping
                .groups
                .iter()
                .enumerate()
                .find(|(_, g)| g.id == *id)
                .ok_or_else(|| AskError::Subject(format!("unknown group {id}")))?;
            b.group(grouping, i);
            format!("group {}. {}", i + 1, group.title)
        }
        Subject::Hunk(hash) => {
            let h = src
                .diff
                .hunks
                .iter()
                .position(|h| h.content_hash == *hash)
                .ok_or_else(|| AskError::Subject("this hunk is not in the review".into()))?;
            b.hunk_subject(h);
            let hunk = &src.diff.hunks[h];
            format!(
                "hunk {}:{}",
                src.diff.files[hunk.file].path,
                hunk.new_range.start.max(1)
            )
        }
    };
    let instructions = match instructions.map(str::trim).filter(|i| !i.is_empty()) {
        Some(i) => format!(
            "\n# Project conventions\n\nThe team describes its architecture as follows.\n\n{}\n",
            truncate(i, MAX_INSTRUCTIONS)
        ),
        None => String::new(),
    };
    let text = TEMPLATE
        .replace("{{language}}", language)
        .replace("{{instructions}}", &instructions)
        .replace("{{question}}", question.trim())
        .replace("{{context}}", b.out.trim_end());
    Ok(Prompt {
        subject: subject.clone(),
        label,
        question: question.trim().to_string(),
        text,
        refs: b.refs,
    })
}

struct Builder<'a> {
    src: &'a Sources<'a>,
    out: String,
    refs: RefIndex,
    files: HashMap<(String, Side), Option<Rc<Vec<String>>>>,
}

fn kind_name(k: SymbolKind) -> String {
    format!("{k:?}").to_lowercase()
}

fn truncate(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_string(),
    }
}

impl Builder<'_> {
    fn full(&self) -> bool {
        self.out.len() >= MAX_CONTEXT
    }

    fn line(&mut self, s: &str) {
        self.out.push_str(s);
        self.out.push('\n');
    }

    fn file(&mut self, path: &str, side: Side) -> Option<Rc<Vec<String>>> {
        let key = (path.to_string(), side);
        if let Some(f) = self.files.get(&key) {
            return f.clone();
        }
        let rev = match side {
            Side::New => self.src.head_sha,
            Side::Old => self.src.base_sha,
        };
        let f =
            (self.src.read)(rev, path).map(|t| Rc::new(t.lines().map(str::to_string).collect()));
        self.files.insert(key, f.clone());
        f
    }

    /// Lines `from..=to` of `path`, numbered, under a `### path:from-to` title.
    fn code(&mut self, path: &str, side: Side, from: u32, to: u32, note: &str) -> bool {
        if self.full() {
            return false;
        }
        let Some(lines) = self.file(path, side) else {
            return false;
        };
        let from = from.max(1);
        let to = to.min(lines.len() as u32);
        if from > to {
            return false;
        }
        let rev = if side == Side::Old {
            ", base revision"
        } else {
            ""
        };
        let _ = writeln!(self.out, "### {path}:{from}-{to}{rev}{note}");
        for n in from..=to {
            let _ = writeln!(self.out, "{n:>5} | {}", lines[(n - 1) as usize]);
            self.refs.add(path, side, n);
        }
        self.out.push('\n');
        true
    }

    /// A hunk with both line numbers.
    fn hunk(&mut self, h: usize) {
        if self.full() {
            return;
        }
        let diff = self.src.diff;
        let hunk = &diff.hunks[h];
        let file = &diff.files[hunk.file];
        let status = format!("{:?}", file.status).to_lowercase();
        let old = file.old_path.as_deref().unwrap_or(&file.path);
        let _ = writeln!(
            self.out,
            "### diff of {} ({status}) @@ -{},{} +{},{} @@ {}",
            file.display_path(),
            hunk.old_range.start,
            hunk.old_range.len,
            hunk.new_range.start,
            hunk.new_range.len,
            hunk.section.trim()
        );
        self.out.truncate(self.out.trim_end().len());
        self.out.push('\n');
        for l in &hunk.lines {
            let num = |n: Option<u32>| n.map(|n| n.to_string()).unwrap_or_default();
            let sign = match l.kind {
                LineKind::Added => '+',
                LineKind::Removed => '-',
                LineKind::Context => ' ',
            };
            let _ = writeln!(
                self.out,
                "{:>5} {:>5} | {sign}{}",
                num(l.old_line),
                num(l.new_line),
                l.text
            );
            if let Some(n) = l.new_line {
                self.refs.add(&file.path, Side::New, n);
            }
            if let Some(n) = l.old_line
                && l.kind == LineKind::Removed
            {
                self.refs.add(old, Side::Old, n);
            }
        }
        self.out.push('\n');
    }

    /// A few lines around `line` of `path` (head).
    fn around(&mut self, path: &str, line: u32, before: u32, after: u32) {
        self.code(
            path,
            Side::New,
            line.saturating_sub(before),
            line + after,
            "",
        );
    }

    fn location(&mut self, g: &Graph, s: SymIdx, line: u32) -> String {
        let sym = g.symbol(s);
        let side = if sym.removed { Side::Old } else { Side::New };
        self.refs.add(&sym.file, side, line);
        format!("{}:{line}", sym.file)
    }

    fn symbol(&mut self, g: &Graph, s: SymIdx, with_hunks: bool) {
        let sym = g.symbol(s);
        let status = if sym.removed {
            "removed by this review"
        } else if sym.changed {
            "changed by this review"
        } else if g.is_file_changed(&sym.file) {
            "unchanged, in a file this review changes"
        } else {
            "unchanged, in a file this review does not touch"
        };
        let _ = writeln!(
            self.out,
            "## Subject: {} `{}` at {}:{} ({status})",
            kind_name(sym.kind),
            g.display_name(s),
            sym.file,
            sym.line
        );
        if !sym.roles.is_empty() {
            let roles: Vec<String> = sym
                .roles
                .iter()
                .map(|r| format!("{r:?}").to_lowercase())
                .collect();
            let _ = writeln!(self.out, "Roles: {}", roles.join(", "));
        }
        for (k, v) in &sym.tags {
            let _ = writeln!(self.out, "Tag {k}: {v}");
        }
        self.out.push('\n');

        let side = if sym.removed { Side::Old } else { Side::New };
        let end = sym.span.end.min(sym.span.start + MAX_SYMBOL_LINES - 1);
        let cut = if end < sym.span.end {
            format!(" (first {MAX_SYMBOL_LINES} lines of {})", sym.span.lines())
        } else {
            String::new()
        };
        if sym.kind != SymbolKind::File {
            self.line("## Code");
            let file = sym.file.clone();
            self.code(&file, side, sym.span.start, end, &cut);
        }

        let hunks = g.hunks_of_symbol(s);
        if with_hunks && !hunks.is_empty() {
            self.line("## Changes of this review in it");
            for &h in hunks.iter().take(MAX_SYMBOL_HUNKS) {
                self.hunk(h);
            }
            if hunks.len() > MAX_SYMBOL_HUNKS {
                let _ = writeln!(
                    self.out,
                    "({} more hunks not shown)\n",
                    hunks.len() - MAX_SYMBOL_HUNKS
                );
            }
        }
        if sym.removed {
            return;
        }

        let is_test = |l: &Link| g.symbol(l.symbol).has_role(Role::Test);
        let callers: Vec<Link> = g.callers(s).into_iter().filter(|l| !is_test(l)).collect();
        let mut tests = g.tests_of(s);
        for l in g.callers(s).into_iter().filter(is_test) {
            if !tests.iter().any(|t| t.symbol == l.symbol) {
                tests.push(l);
            }
        }
        self.links(
            g,
            "Called by (static analysis)",
            &callers,
            true,
            MAX_CALLERS,
            2,
        );
        self.links(g, "Calls", &g.callees(s), false, MAX_CALLEES, 0);
        self.links(g, "Tests exercising it", &tests, true, MAX_TESTS, 0);

        // Other edges: framework wiring first, then type uses and imports.
        let rank = |k: EdgeKind| match k {
            EdgeKind::Injects
            | EdgeKind::HttpCalls
            | EdgeKind::Configures
            | EdgeKind::Routes
            | EdgeKind::Publishes => 0,
            EdgeKind::Overrides | EdgeKind::Inherits => 1,
            _ => 2,
        };
        let mut other: Vec<(u8, String, SymIdx, u32, f32)> = Vec::new();
        for e in g.edges_to(s) {
            if !matches!(e.kind, EdgeKind::Calls | EdgeKind::Tests) {
                let line = if e.line > 0 {
                    e.line
                } else {
                    g.symbol(e.from).line
                };
                let label = format!("← {} from", edge_name(e.kind));
                other.push((rank(e.kind), label, e.from, line, e.confidence));
            }
        }
        for e in g.edges_from(s) {
            if !matches!(e.kind, EdgeKind::Calls | EdgeKind::Tests) {
                let label = format!("{} →", edge_name(e.kind));
                other.push((rank(e.kind), label, e.to, g.symbol(e.to).line, e.confidence));
            }
        }
        other.sort_by(|a, b| a.0.cmp(&b.0).then(b.4.total_cmp(&a.4)));
        if !other.is_empty() && !self.full() {
            self.line("## Other links (static analysis)");
            for (_, label, t, line, conf) in other.iter().take(MAX_OTHER_LINKS) {
                let at = self.location(g, *t, *line);
                let _ = writeln!(
                    self.out,
                    "- {label} `{}` at {at}, confidence {conf:.2}",
                    g.display_name(*t)
                );
            }
            if other.len() > MAX_OTHER_LINKS {
                let _ = writeln!(self.out, "- … {} more", other.len() - MAX_OTHER_LINKS);
            }
            self.out.push('\n');
        }
    }

    /// A list of linked symbols; with `context > 0`, the code around each
    /// reference.
    fn links(
        &mut self,
        g: &Graph,
        title: &str,
        links: &[Link],
        at_reference: bool,
        max: usize,
        context: u32,
    ) {
        if self.full() {
            return;
        }
        if links.is_empty() {
            let _ = writeln!(self.out, "## {title}: none found\n");
            return;
        }
        let _ = writeln!(self.out, "## {title} ({})", links.len());
        for l in links.iter().take(max) {
            let other = g.symbol(l.symbol);
            let line = if at_reference && l.line > 0 {
                l.line
            } else {
                other.line
            };
            let at = self.location(g, l.symbol, line);
            let mut facts = vec![format!("confidence {:.2}", l.confidence)];
            if let Some(v) = l.via {
                facts.push(format!("via `{}`", g.display_name(v)));
            }
            facts.push(
                if other.changed {
                    "changed by this review"
                } else if g.is_file_changed(&other.file) {
                    "in a changed file"
                } else {
                    "in an untouched file"
                }
                .to_string(),
            );
            let _ = writeln!(
                self.out,
                "- `{}` ({}) at {at}: {}",
                g.display_name(l.symbol),
                kind_name(other.kind),
                facts.join(", ")
            );
            if context > 0 && !other.removed {
                let file = other.file.clone();
                self.around(&file, line, context, context);
            }
        }
        if links.len() > max {
            let _ = writeln!(self.out, "- … {} more", links.len() - max);
        }
        self.out.push('\n');
    }

    /// Titles and summaries of the groups containing `hunks`.
    fn groups_of(&mut self, hunks: &[usize]) {
        let Some(grouping) = self.src.grouping else {
            return;
        };
        let mut seen = BTreeSet::new();
        for &h in hunks {
            if let Some(i) = grouping.group_of_hunk(h)
                && seen.insert(i)
            {
                let g = &grouping.groups[i];
                let _ = writeln!(
                    self.out,
                    "## Functional group of this change: \"{}\"\n{}\n",
                    g.title, g.summary
                );
            }
        }
    }

    fn group(&mut self, grouping: &Grouping, i: usize) {
        let group = &grouping.groups[i];
        let _ = writeln!(
            self.out,
            "## Subject: group {} of {} in reading order: \"{}\"\n{}",
            i + 1,
            grouping.groups.len(),
            group.title,
            group.summary
        );
        let layers: Vec<String> = group
            .layers
            .iter()
            .map(|l| format!("{} ({} hunks)", l.name, l.hunk_ids.len()))
            .collect();
        let _ = writeln!(self.out, "Layers: {}\n", layers.join(", "));

        if let Some(g) = self.src.graph {
            let mut syms: Vec<SymIdx> = Vec::new();
            for &h in &group.hunk_ids {
                for &s in g.symbols_of_hunk(h) {
                    if g.symbol(s).kind != SymbolKind::File && !syms.contains(&s) {
                        syms.push(s);
                    }
                }
            }
            if !syms.is_empty() {
                let _ = writeln!(
                    self.out,
                    "## Symbols changed by this group ({})",
                    syms.len()
                );
                for &s in syms.iter().take(MAX_GROUP_SYMBOLS) {
                    let sym = g.symbol(s);
                    let at = self.location(g, s, sym.line);
                    let callers: Vec<Link> = g
                        .callers(s)
                        .into_iter()
                        .filter(|l| !g.symbol(l.symbol).has_role(Role::Test))
                        .collect();
                    let untouched = callers
                        .iter()
                        .filter(|l| !g.is_file_changed(&g.symbol(l.symbol).file))
                        .count();
                    let mut who = Vec::new();
                    for l in callers.iter().take(3) {
                        let line = if l.line > 0 {
                            l.line
                        } else {
                            g.symbol(l.symbol).line
                        };
                        let at = self.location(g, l.symbol, line);
                        who.push(format!("`{}` at {at}", g.display_name(l.symbol)));
                    }
                    let removed = if sym.removed { ", removed" } else { "" };
                    let _ = write!(
                        self.out,
                        "- `{}` ({}{removed}) at {at}: {} caller(s), {untouched} in untouched files",
                        g.display_name(s),
                        kind_name(sym.kind),
                        callers.len()
                    );
                    if !who.is_empty() {
                        let _ = write!(self.out, "; called by {}", who.join(", "));
                    }
                    self.out.push('\n');
                }
                if syms.len() > MAX_GROUP_SYMBOLS {
                    let _ = writeln!(self.out, "- … {} more", syms.len() - MAX_GROUP_SYMBOLS);
                }
                self.out.push('\n');
            }
        }

        self.line("## Hunks of the group, by layer");
        let mut shown = 0;
        for layer in &group.layers {
            if self.full() {
                break;
            }
            let _ = writeln!(self.out, "#### layer {}", layer.name);
            for &h in &layer.hunk_ids {
                if self.full() {
                    break;
                }
                self.hunk(h);
                shown += 1;
            }
        }
        if shown < group.hunk_ids.len() {
            let _ = writeln!(
                self.out,
                "({} more hunks not shown: context budget reached)",
                group.hunk_ids.len() - shown
            );
        }
        let files: Vec<String> = group
            .file_ids
            .iter()
            .map(|&f| self.src.diff.files[f].display_path())
            .collect();
        if !files.is_empty() {
            let _ = writeln!(
                self.out,
                "Files without textual changes: {}",
                files.join(", ")
            );
        }
    }

    fn hunk_subject(&mut self, h: usize) {
        let diff = self.src.diff;
        let file = &diff.files[diff.hunks[h].file];
        let _ = writeln!(
            self.out,
            "## Subject: a hunk of {} ({})\n",
            file.display_path(),
            format!("{:?}", file.status).to_lowercase()
        );
        self.hunk(h);
        if let Some(g) = self.src.graph
            && let Some(&s) = g
                .symbols_of_hunk(h)
                .iter()
                .find(|&&s| g.symbol(s).kind != SymbolKind::File)
        {
            self.line("# The symbol this hunk changes");
            self.symbol(g, s, false);
        }
        self.groups_of(&[h]);
    }
}

fn edge_name(k: EdgeKind) -> String {
    let debug = format!("{k:?}");
    let mut out = String::new();
    for (i, c) in debug.chars().enumerate() {
        if c.is_uppercase() && i > 0 {
            out.push(' ');
        }
        out.extend(c.to_lowercase());
    }
    out
}

// ----- references -----------------------------------------------------------

/// Finds the `path:line` references of `text` (bracketed or not, `-end` and
/// ` (old)` accepted) and checks them against `refs`.
pub fn parse_refs(text: &str, refs: &RefIndex) -> Vec<CodeRef> {
    let tokens: Vec<(usize, &str)> = tokens(text);
    let mut out = Vec::new();
    for (i, &(offset, token)) in tokens.iter().enumerate() {
        let lead = token.len() - token.trim_start_matches(['[', '(', '`', '"', '\'']).len();
        let core = token[lead..]
            .trim_end_matches([']', ')', '`', '"', '\'', ',', '.', ';', ':', '!', '?']);
        let Some((path, nums)) = core.rsplit_once(':') else {
            continue;
        };
        let (line, end_line) = match nums.split_once('-') {
            Some((a, b)) => (a.parse::<u32>().ok(), b.parse::<u32>().ok()),
            None => (nums.parse::<u32>().ok(), None),
        };
        let Some(line) = line.filter(|&l| l > 0) else {
            continue;
        };
        if path.is_empty()
            || !(path.contains('.') || path.contains('/'))
            || path.contains("://")
            || path.contains(['[', ']', '(', ')'])
        {
            continue;
        }
        let old_mark = tokens
            .get(i + 1)
            .is_some_and(|(_, t)| t.trim_start_matches('[').starts_with("(old)"));
        let start = offset + lead;
        let end = start + core.len();
        let resolved = refs.resolve(path);
        let mut side = if old_mark { Side::Old } else { Side::New };
        let valid = match resolved {
            Some(p) => {
                if refs.contains(p, side, line) {
                    true
                } else if !old_mark && refs.contains(p, Side::Old, line) {
                    side = Side::Old;
                    true
                } else {
                    false
                }
            }
            None => false,
        };
        out.push(CodeRef {
            start,
            end,
            raw_path: path.to_string(),
            path: resolved.unwrap_or(path).to_string(),
            line,
            end_line: end_line.filter(|&e| e >= line),
            side,
            valid,
        });
    }
    out
}

/// Whitespace-separated tokens with their byte offset.
fn tokens(text: &str) -> Vec<(usize, &str)> {
    let mut out = Vec::new();
    let mut start = None;
    for (i, c) in text.char_indices() {
        if c.is_whitespace() {
            if let Some(s) = start.take() {
                out.push((s, &text[s..i]));
            }
        } else if start.is_none() {
            start = Some(i);
        }
    }
    if let Some(s) = start {
        out.push((s, &text[s..]));
    }
    out
}

// ----- asking and caching ---------------------------------------------------

/// How to ask.
#[derive(Debug, Clone)]
pub struct Params {
    pub model: Option<String>,
    /// Where the LLM runs from.
    pub cwd: PathBuf,
    /// Cache directory, see [`cache_dir`]; `None`: no cache.
    pub cache_dir: Option<PathBuf>,
    pub use_cache: bool,
    pub head_sha: String,
}

/// Key of an answer: prompt version, model and the whole prompt (subject,
/// question, context, language and instructions).
pub fn cache_key(prompt: &Prompt, model: Option<&str>) -> String {
    let mut h = blake3::Hasher::new();
    h.update(format!("ask v{ASK_PROMPT_VERSION}\0{}\0", model.unwrap_or("")).as_bytes());
    h.update(prompt.text.as_bytes());
    h.finalize().to_hex()[..24].to_string()
}

/// `.git/survol/cache/<head_sha>/ask/`
pub fn cache_dir(survol_dir: &Path, head_sha: &str) -> PathBuf {
    survol_dir.join("cache").join(head_sha).join("ask")
}

/// Asks `prompt`, from the cache when possible.
pub fn ask(prompt: &Prompt, llm: &dyn LlmProvider, p: &Params) -> Result<Answer, AskError> {
    let key = cache_key(prompt, p.model.as_deref());
    let path = p.cache_dir.as_ref().map(|d| d.join(format!("{key}.json")));
    if p.use_cache
        && let Some(path) = &path
        && let Ok(bytes) = std::fs::read(path)
        && let Ok(mut a) = serde_json::from_slice::<Answer>(&bytes)
    {
        a.from_cache = true;
        return Ok(a);
    }
    let raw = llm.complete(&LlmRequest {
        prompt: prompt.text.clone(),
        model: p.model.clone(),
        effort: None,
        cwd: p.cwd.clone(),
    })?;
    let text = strip_fence(raw.trim()).to_string();
    if text.is_empty() {
        return Err(AskError::Empty);
    }
    let refs = parse_refs(&text, &prompt.refs);
    let answer = Answer {
        subject: prompt.subject.clone(),
        label: prompt.label.clone(),
        question: prompt.question.clone(),
        text,
        refs,
        model: p.model.clone(),
        head_sha: p.head_sha.clone(),
        prompt_version: ASK_PROMPT_VERSION,
        key,
        asked_at: now(),
        from_cache: false,
    };
    if let Some(path) = &path {
        write_json(path, &answer)?;
    }
    Ok(answer)
}

/// A whole answer wrapped in a Markdown fence despite the instructions.
fn strip_fence(s: &str) -> &str {
    match s.strip_prefix("```") {
        Some(rest) => {
            let rest = rest.split_once('\n').map_or("", |(_, r)| r);
            rest.trim_end().strip_suffix("```").unwrap_or(rest).trim()
        }
        None => s,
    }
}

pub(crate) fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

pub(crate) fn write_json<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(
        &tmp,
        serde_json::to_vec_pretty(value).map_err(io::Error::other)?,
    )?;
    std::fs::rename(tmp, path)
}

// ----- history --------------------------------------------------------------

/// `.git/survol/reviews/<key>/questions.json`: the questions asked during a
/// review, oldest first.
pub fn history_path(survol_dir: &Path, state_key: &str) -> PathBuf {
    survol_dir
        .join("reviews")
        .join(state_key)
        .join("questions.json")
}

pub fn load_history(path: &Path) -> io::Result<Vec<Answer>> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(io::Error::other),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e),
    }
}

/// Adds `answer` at the end of the history (moving it there if it was
/// already asked).
pub fn append_history(path: &Path, answer: &Answer) -> io::Result<Vec<Answer>> {
    let mut all = load_history(path)?;
    all.retain(|a| a.key != answer.key);
    let mut a = answer.clone();
    a.from_cache = false;
    all.push(a);
    write_json(path, &all)?;
    Ok(all)
}

#[cfg(test)]
mod tests;
