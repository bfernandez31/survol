//! Review comments: local drafts, their anchoring in the diff, GitLab
//! positions, and publication.
//!
//! Drafts live in `.git/survol/reviews/<key>/comments.json`, anchored by
//! file, side, the hunk's `content_hash`, the line's index in the hunk and
//! its text, never by line number alone: after new commits, a draft follows
//! its line (same hunk, or the only / closest line with the same text in the
//! file) or is reported as stale. Positions are computed from the current
//! diff when publishing. Publishing creates GitLab draft notes then publishes
//! them at once (`bulk_publish`); instances without draft notes get regular
//! discussions, one by one, after an explicit confirmation.

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::forge::{
    Capabilities, Forge, ForgeError, LinePoint, LineRange, NewComment, Position,
    draft_note_payload, post_comment_request,
};
use crate::model::{Diff, LineKind, Side};

const FORMAT_VERSION: u32 = 1;

/// A line a comment is attached to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LineAnchor {
    /// File path (the new path; the old one for a deleted file).
    pub path: String,
    /// `old` for a removed line, `new` otherwise.
    pub side: Side,
    /// Line number on that side when the comment was written (informative).
    pub line: u32,
    pub hunk_hash: String,
    /// Index of the line in the hunk.
    pub index: usize,
    pub kind: LineKind,
    pub text: String,
}

/// What a comment is about.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "on", rename_all = "snake_case")]
pub enum Anchor {
    /// A line, or the range `start..=line` of one hunk.
    Line {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        start: Option<LineAnchor>,
        line: LineAnchor,
    },
    /// A whole file.
    File {
        path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        old_path: Option<String>,
    },
    /// A reply to an existing discussion of the merge request.
    Reply {
        discussion: String,
        /// Who started the discussion, for display.
        #[serde(default)]
        author: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Draft {
    /// Local id, unique in the store.
    pub id: u64,
    pub body: String,
    pub anchor: Anchor,
    /// Seconds since the Unix epoch.
    pub created_at: u64,
    pub updated_at: u64,
    /// Draft note already created on the forge by an interrupted
    /// publication: not created twice.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_id: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published_at: Option<u64>,
}

/// The local comments of a review.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommentStore {
    pub version: u32,
    pub next_id: u64,
    pub drafts: Vec<Draft>,
    /// Overall comment of the review, published with the drafts.
    #[serde(default)]
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary_remote_id: Option<u64>,
    /// Drafts already published, oldest first.
    #[serde(default)]
    pub published: Vec<Draft>,
}

/// `.git/survol/reviews/<key>/comments.json`
pub fn comments_path(survol_dir: &Path, state_key: &str) -> PathBuf {
    survol_dir
        .join("reviews")
        .join(state_key)
        .join("comments.json")
}

impl CommentStore {
    pub fn load(path: &Path) -> io::Result<Self> {
        match std::fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(io::Error::other),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e),
        }
    }

    pub fn save(&mut self, path: &Path) -> io::Result<()> {
        self.version = FORMAT_VERSION;
        crate::ask::write_json(path, self)
    }

    /// Adds a draft; returns its id.
    pub fn add(&mut self, anchor: Anchor, body: &str) -> u64 {
        self.next_id = self.next_id.max(1);
        let id = self.next_id;
        self.next_id += 1;
        let now = crate::ask::now();
        self.drafts.push(Draft {
            id,
            body: body.trim_end().to_string(),
            anchor,
            created_at: now,
            updated_at: now,
            remote_id: None,
            published_at: None,
        });
        id
    }

    pub fn get(&self, id: u64) -> Option<&Draft> {
        self.drafts.iter().find(|d| d.id == id)
    }

    /// Changes the body of a draft; an empty body deletes it.
    pub fn update(&mut self, id: u64, body: &str) {
        if body.trim().is_empty() {
            return self.remove(id);
        }
        if let Some(d) = self.drafts.iter_mut().find(|d| d.id == id) {
            d.body = body.trim_end().to_string();
            d.updated_at = crate::ask::now();
        }
    }

    pub fn remove(&mut self, id: u64) {
        self.drafts.retain(|d| d.id != id);
    }

    /// Nothing to publish.
    pub fn is_empty(&self) -> bool {
        self.drafts.is_empty() && self.summary.trim().is_empty()
    }
}

// ----- anchoring ------------------------------------------------------------

/// Where a draft lands in the current diff.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "at", rename_all = "snake_case")]
pub enum Placement {
    Line {
        file: usize,
        hunk: usize,
        /// Index of the (last) line in the hunk.
        line: usize,
        /// First line of a range.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        start: Option<usize>,
        /// Found by its text, not in its original hunk (the hunk changed).
        moved: bool,
    },
    File {
        file: usize,
    },
    /// A reply: shown with its discussion.
    Reply,
    /// The line or file is no longer in the diff.
    Stale,
}

impl Placement {
    pub fn is_stale(self) -> bool {
        self == Placement::Stale
    }
}

/// Anchor of line `line` (index) of `hunk`.
pub fn line_anchor(diff: &Diff, hunk: usize, line: usize) -> LineAnchor {
    let h = &diff.hunks[hunk];
    let l = &h.lines[line];
    let side = if l.kind == LineKind::Removed {
        Side::Old
    } else {
        Side::New
    };
    LineAnchor {
        path: diff.files[h.file].path.clone(),
        side,
        line: match side {
            Side::Old => l.old_line,
            Side::New => l.new_line,
        }
        .unwrap_or(0),
        hunk_hash: h.content_hash.clone(),
        index: line,
        kind: l.kind,
        text: l.text.clone(),
    }
}

/// Anchor of the whole file `file`.
pub fn file_anchor(diff: &Diff, file: usize) -> Anchor {
    let f = &diff.files[file];
    Anchor::File {
        path: f.path.clone(),
        old_path: f.old_path.clone(),
    }
}

/// Finds `anchor` in `diff`.
pub fn place(anchor: &Anchor, diff: &Diff) -> Placement {
    match anchor {
        Anchor::File { path, .. } => diff
            .files
            .iter()
            .position(|f| &f.path == path)
            .map_or(Placement::Stale, |file| Placement::File { file }),
        Anchor::Line { start, line } => place_line(diff, start.as_ref(), line),
        Anchor::Reply { .. } => Placement::Reply,
    }
}

fn same_line(diff: &Diff, hunk: usize, i: usize, a: &LineAnchor) -> bool {
    diff.hunks[hunk]
        .lines
        .get(i)
        .is_some_and(|l| l.kind == a.kind && l.text == a.text)
}

fn place_line(diff: &Diff, start: Option<&LineAnchor>, a: &LineAnchor) -> Placement {
    // Same hunk (same content): same index.
    if let Some(h) = diff
        .hunks
        .iter()
        .position(|h| h.content_hash == a.hunk_hash)
        && same_line(diff, h, a.index, a)
    {
        let file = diff.hunks[h].file;
        let start = start
            .filter(|s| s.index < a.index && same_line(diff, h, s.index, s))
            .map(|s| s.index);
        return Placement::Line {
            file,
            hunk: h,
            line: a.index,
            start,
            moved: false,
        };
    }
    // The hunk changed: the line with the same text in the same file,
    // closest to where it was.
    let Some(file) = diff.files.iter().position(|f| f.path == a.path) else {
        return Placement::Stale;
    };
    let mut best: Option<(u32, usize, usize)> = None;
    let mut tie = false;
    for h in diff.file_hunks(file) {
        for (i, l) in h.lines.iter().enumerate() {
            if l.kind != a.kind || l.text != a.text {
                continue;
            }
            let n = match a.side {
                Side::Old => l.old_line,
                Side::New => l.new_line,
            }
            .unwrap_or(0);
            let dist = n.abs_diff(a.line);
            match best {
                Some((d, _, _)) if d < dist => {}
                Some((d, _, _)) if d == dist => tie = true,
                _ => {
                    best = Some((dist, h.id, i));
                    tie = false;
                }
            }
        }
    }
    match best {
        Some((_, hunk, line)) if !tie => {
            // Keep a range when its first line is still at the same distance.
            let start = start.and_then(|s| {
                let i = line.checked_sub(a.index.checked_sub(s.index)?)?;
                (i < line && same_line(diff, hunk, i, s)).then_some(i)
            });
            Placement::Line {
                file,
                hunk,
                line,
                start,
                moved: true,
            }
        }
        _ => Placement::Stale,
    }
}

/// The line of the diff a forge position designates, if it is shown:
/// `(file, Some((hunk, line index)))`, or `(file, None)` when only the file
/// is in the diff.
pub fn place_position(pos: &Position, diff: &Diff) -> Option<(usize, Option<(usize, usize)>)> {
    let file = diff.files.iter().position(|f| {
        f.path == pos.new_path || f.old_path.as_deref().unwrap_or(&f.path) == pos.old_path
    })?;
    let at = diff.file_hunks(file).find_map(|h| {
        let i = h
            .lines
            .iter()
            .position(|l| match (pos.new_line, pos.old_line) {
                (Some(n), _) => l.new_line == Some(n) && l.kind != LineKind::Removed,
                (None, Some(o)) => l.old_line == Some(o) && l.kind == LineKind::Removed,
                (None, None) => false,
            })?;
        Some((h.id, i))
    });
    Some((file, at))
}

// ----- positions ------------------------------------------------------------

/// The three SHAs of a merge request version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Shas {
    pub base: String,
    pub start: String,
    pub head: String,
}

fn paths(diff: &Diff, file: usize) -> (String, String) {
    let f = &diff.files[file];
    let new = f.path.clone();
    let old = f.old_path.clone().unwrap_or_else(|| new.clone());
    (old, new)
}

/// GitLab's line counters: for each line of the hunk, the old and new line
/// numbers the diff parser is at (an added line keeps the old counter, a
/// removed line the new one).
fn counters(diff: &Diff, hunk: usize) -> Vec<(u32, u32)> {
    let h = &diff.hunks[hunk];
    let (mut old, mut new) = (h.old_range.start, h.new_range.start);
    h.lines
        .iter()
        .map(|l| {
            let at = (old, new);
            match l.kind {
                LineKind::Added => new += 1,
                LineKind::Removed => old += 1,
                LineKind::Context => {
                    old += 1;
                    new += 1;
                }
            }
            at
        })
        .collect()
}

/// `<sha1(path)>_<old>_<new>`: GitLab's line code.
pub fn line_code(path: &str, old: u32, new: u32) -> String {
    format!("{}_{old}_{new}", sha1_hex(path.as_bytes()))
}

fn line_point(diff: &Diff, file: usize, hunk: usize, line: usize) -> LinePoint {
    let l = &diff.hunks[hunk].lines[line];
    let (old, new) = counters(diff, hunk)[line];
    LinePoint {
        line_code: line_code(&diff.files[file].path, old, new),
        kind: Some(
            if l.kind == LineKind::Added {
                "new"
            } else {
                "old"
            }
            .to_string(),
        ),
        old_line: l.old_line.filter(|_| l.kind != LineKind::Added),
        new_line: l.new_line.filter(|_| l.kind != LineKind::Removed),
    }
}

/// Position of line `line` of `hunk` (the range `start..=line` if given):
/// `new_line` for an added line, `old_line` for a removed one, both for an
/// unchanged line.
pub fn line_position(
    diff: &Diff,
    hunk: usize,
    line: usize,
    start: Option<usize>,
    shas: &Shas,
) -> Position {
    let file = diff.hunks[hunk].file;
    let (old_path, new_path) = paths(diff, file);
    let l = &diff.hunks[hunk].lines[line];
    let line_range = start.filter(|&s| s < line).map(|s| LineRange {
        start: line_point(diff, file, hunk, s),
        end: line_point(diff, file, hunk, line),
    });
    Position {
        position_type: "text".into(),
        base_sha: shas.base.clone(),
        start_sha: shas.start.clone(),
        head_sha: shas.head.clone(),
        old_path,
        new_path,
        old_line: l.old_line.filter(|_| l.kind != LineKind::Added),
        new_line: l.new_line.filter(|_| l.kind != LineKind::Removed),
        line_range,
    }
}

/// Position of a comment on the whole file.
pub fn file_position(diff: &Diff, file: usize, shas: &Shas) -> Position {
    let (old_path, new_path) = paths(diff, file);
    Position {
        position_type: "file".into(),
        base_sha: shas.base.clone(),
        start_sha: shas.start.clone(),
        head_sha: shas.head.clone(),
        old_path,
        new_path,
        old_line: None,
        new_line: None,
        line_range: None,
    }
}

// ----- publication ----------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// Draft notes, then one `bulk_publish`: the review appears at once.
    Drafts,
    /// No draft notes on the instance: each comment is posted right away.
    Direct,
}

/// A comment to send.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Planned {
    /// Local draft; `None` for the summary.
    pub draft: Option<u64>,
    /// Short description (`path:line`, `path`, `summary`).
    pub what: String,
    pub comment: NewComment,
    /// Already created as a draft note by an interrupted publication.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_id: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Plan {
    pub mode: Mode,
    pub comments: Vec<Planned>,
    /// Stale drafts, not sent: `(id, reason)`.
    pub skipped: Vec<(u64, String)>,
}

/// An HTTP request of a plan, for `--dry-run` and confirmations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Request {
    pub method: String,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<serde_json::Value>,
}

/// `path:line` / `path:first-last` of a placed draft, on its side.
pub fn describe(diff: &Diff, p: Placement) -> String {
    match p {
        Placement::Line {
            hunk, line, start, ..
        } => {
            let h = &diff.hunks[hunk];
            let num = |i: usize| {
                let l = &h.lines[i];
                match l.kind {
                    LineKind::Removed => format!("{} (old)", l.old_line.unwrap_or(0)),
                    _ => l.new_line.unwrap_or(0).to_string(),
                }
            };
            let path = &diff.files[h.file].path;
            match start {
                Some(s) => format!("{path}:{}-{}", num(s), num(line)),
                None => format!("{path}:{}", num(line)),
            }
        }
        Placement::File { file } => diff.files[file].path.clone(),
        Placement::Reply => "reply".into(),
        Placement::Stale => "stale".into(),
    }
}

/// What publishing `store` would send.
pub fn plan(store: &CommentStore, diff: &Diff, shas: &Shas, caps: &Capabilities) -> Plan {
    let mode = if caps.draft_notes {
        Mode::Drafts
    } else {
        Mode::Direct
    };
    let mut comments = Vec::new();
    let mut skipped = Vec::new();
    for d in &store.drafts {
        let p = place(&d.anchor, diff);
        if let Anchor::Reply { discussion, author } = &d.anchor {
            comments.push(Planned {
                draft: Some(d.id),
                what: format!("reply to @{author}"),
                comment: NewComment {
                    body: d.body.clone(),
                    position: None,
                    in_reply_to: Some(discussion.clone()),
                },
                remote_id: d.remote_id.filter(|_| mode == Mode::Drafts),
            });
            continue;
        }
        let (position, body) = match p {
            Placement::Stale => {
                skipped.push((d.id, "its line is no longer in the diff".to_string()));
                continue;
            }
            Placement::Line {
                hunk, line, start, ..
            } => (
                Some(line_position(diff, hunk, line, start, shas)),
                d.body.clone(),
            ),
            Placement::File { file } if caps.file_comments => {
                (Some(file_position(diff, file, shas)), d.body.clone())
            }
            // No file comments on this instance: a general comment naming the file.
            Placement::File { file } => (
                None,
                format!("**`{}`**\n\n{}", diff.files[file].path, d.body),
            ),
            Placement::Reply => continue,
        };
        comments.push(Planned {
            draft: Some(d.id),
            what: describe(diff, p),
            comment: NewComment {
                body,
                position,
                in_reply_to: None,
            },
            remote_id: d.remote_id.filter(|_| mode == Mode::Drafts),
        });
    }
    if !store.summary.trim().is_empty() {
        comments.push(Planned {
            draft: None,
            what: "summary".into(),
            comment: NewComment {
                body: store.summary.trim_end().to_string(),
                position: None,
                in_reply_to: None,
            },
            remote_id: store.summary_remote_id.filter(|_| mode == Mode::Drafts),
        });
    }
    Plan {
        mode,
        comments,
        skipped,
    }
}

impl Plan {
    /// The HTTP requests, in order, for the project `project` (URL-encoded
    /// path or id) and merge request `iid`.
    pub fn requests(&self, project: &str, iid: &str) -> Vec<Request> {
        let base = format!("/projects/{project}/merge_requests/{iid}");
        let mut out: Vec<Request> = self
            .comments
            .iter()
            .filter(|c| c.remote_id.is_none())
            .map(|c| match self.mode {
                Mode::Drafts => Request {
                    method: "POST".into(),
                    path: format!("{base}/draft_notes"),
                    body: Some(draft_note_payload(&c.comment)),
                },
                Mode::Direct => {
                    let (sub, body) = post_comment_request(&c.comment);
                    Request {
                        method: "POST".into(),
                        path: format!("{base}{sub}"),
                        body: Some(body),
                    }
                }
            })
            .collect();
        if self.mode == Mode::Drafts && !self.comments.is_empty() {
            out.push(Request {
                method: "POST".into(),
                path: format!("{base}/draft_notes/bulk_publish"),
                body: None,
            });
        }
        out
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PublishError {
    #[error("publishing stopped after {done} of {total} comment(s): {source}")]
    Forge {
        done: usize,
        total: usize,
        source: ForgeError,
    },
    #[error("cannot save the comments: {0}")]
    Io(#[from] io::Error),
}

/// Sends `plan`, saving `store` (at `path`) after each step so that an
/// interrupted publication resumes without duplicates. Published drafts
/// move to [`CommentStore::published`]. Returns the number of comments sent.
pub fn publish(
    forge: &dyn Forge,
    project: &str,
    iid: u64,
    plan: &Plan,
    store: &mut CommentStore,
    path: &Path,
    progress: &mut dyn FnMut(&str),
) -> Result<usize, PublishError> {
    let total = plan.comments.len();
    let fail = |done, source| PublishError::Forge {
        done,
        total,
        source,
    };
    for (i, c) in plan.comments.iter().enumerate() {
        progress(&format!("{}/{total} {}", i + 1, c.what));
        match plan.mode {
            Mode::Drafts => {
                if c.remote_id.is_some() {
                    continue;
                }
                let id = forge
                    .create_draft_note(project, iid, &c.comment)
                    .map_err(|e| fail(i, e))?;
                match c.draft {
                    Some(d) => {
                        if let Some(x) = store.drafts.iter_mut().find(|x| x.id == d) {
                            x.remote_id = Some(id);
                        }
                    }
                    None => store.summary_remote_id = Some(id),
                }
            }
            Mode::Direct => {
                forge
                    .post_comment(project, iid, &c.comment)
                    .map_err(|e| fail(i, e))?;
                mark_published(store, c.draft);
            }
        }
        store.save(path)?;
    }
    if plan.mode == Mode::Drafts && total > 0 {
        progress("publishing the review");
        forge
            .publish_drafts(project, iid)
            .map_err(|e| fail(total, e))?;
        for c in &plan.comments {
            mark_published(store, c.draft);
        }
        store.save(path)?;
    }
    Ok(total)
}

fn mark_published(store: &mut CommentStore, draft: Option<u64>) {
    match draft {
        Some(id) => {
            if let Some(i) = store.drafts.iter().position(|d| d.id == id) {
                let mut d = store.drafts.remove(i);
                d.published_at = Some(crate::ask::now());
                d.remote_id = None;
                store.published.push(d);
            }
        }
        None => {
            store.summary.clear();
            store.summary_remote_id = None;
        }
    }
}

// ----- SHA-1 ----------------------------------------------------------------

/// SHA-1 of `data`, in hex: only for GitLab's `line_code`, not for security.
pub fn sha1_hex(data: &[u8]) -> String {
    let mut h: [u32; 5] = [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0];
    let mut msg = data.to_vec();
    let bits = (data.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bits.to_be_bytes());
    for chunk in msg.chunks(64) {
        let mut w = [0u32; 80];
        for (i, word) in chunk.chunks(4).enumerate() {
            w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let [mut a, mut b, mut c, mut d, mut e] = h;
        for (i, &wi) in w.iter().enumerate() {
            let (f, k) = match i {
                0..20 => ((b & c) | (!b & d), 0x5A827999),
                20..40 => (b ^ c ^ d, 0x6ED9EBA1),
                40..60 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDC),
                _ => (b ^ c ^ d, 0xCA62C1D6),
            };
            let t = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(wi);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = t;
        }
        for (x, y) in h.iter_mut().zip([a, b, c, d, e]) {
            *x = x.wrapping_add(y);
        }
    }
    h.iter().map(|x| format!("{x:08x}")).collect()
}

#[cfg(test)]
mod tests;
