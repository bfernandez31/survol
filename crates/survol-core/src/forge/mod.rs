//! Forge abstraction. GitLab (self-hosted) first; GitHub may come later.

#[cfg(test)]
pub(crate) mod fake;
pub mod gitlab;

use serde::{Deserialize, Serialize};

use crate::model::MergeRequest;

#[derive(Debug, thiserror::Error)]
pub enum ForgeError {
    #[error("no GitLab host configured: set GITLAB_HOST or [gitlab] host in the config")]
    NoHost,
    #[error("no token for {0}: log in with `glab auth login --hostname {0}` or set GITLAB_TOKEN")]
    NoToken(String),
    #[error("cannot read CA certificate {path}: {message}")]
    CaCert { path: String, message: String },
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("{status} on {url}: {body}")]
    Status {
        status: u16,
        url: String,
        body: String,
    },
    #[error("no open merge request for branch `{0}`")]
    NoMergeRequestForBranch(String),
    #[error("cannot determine the project: {0}")]
    Project(String),
}

pub type Result<T> = std::result::Result<T, ForgeError>;

pub trait Forge {
    /// Human-readable server version, e.g. `17.5.1-ee`.
    fn server_version(&self) -> Result<String>;

    fn merge_request(&self, project: &str, iid: u64) -> Result<MergeRequest>;

    /// Open merge request whose source branch is `branch`.
    fn merge_request_for_branch(&self, project: &str, branch: &str) -> Result<u64>;

    /// Every discussion of the merge request (all pages), system notes included.
    fn discussions(&self, project: &str, iid: u64) -> Result<Vec<Discussion>>;

    /// The current user's pending draft notes on the merge request.
    fn draft_notes(&self, project: &str, iid: u64) -> Result<Vec<DraftNote>>;

    /// Creates a draft note (a comment of the pending review); returns its id.
    fn create_draft_note(&self, project: &str, iid: u64, c: &NewComment) -> Result<u64>;

    /// Publishes all the current user's draft notes at once.
    fn publish_drafts(&self, project: &str, iid: u64) -> Result<()>;

    /// Posts a comment right away (instances without draft notes): a new
    /// discussion, or a note of the discussion it replies to.
    fn post_comment(&self, project: &str, iid: u64, c: &NewComment) -> Result<()>;
}

/// Where a comment sits in the diff of a merge request, as GitLab stores it.
/// `text`: a line (`new_line` for an added line, `old_line` for a removed
/// one, both for an unchanged line), optionally a range; `file`: the file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Position {
    pub position_type: String,
    pub base_sha: String,
    pub start_sha: String,
    pub head_sha: String,
    pub old_path: String,
    pub new_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_line: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_line: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_range: Option<LineRange>,
}

/// First and last lines of a multi-line comment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LineRange {
    pub start: LinePoint,
    pub end: LinePoint,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinePoint {
    /// `<sha1 of the path>_<old line>_<new line>`, GitLab's line identifier.
    pub line_code: String,
    /// `new` for an added line, `old` otherwise.
    #[serde(rename = "type", default)]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_line: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_line: Option<u32>,
}

/// A comment to create: a body, and a position for diff comments or the
/// discussion it answers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewComment {
    pub body: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<Position>,
    /// Id of the discussion this comment replies to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_reply_to: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Author {
    pub username: String,
    #[serde(default)]
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Note {
    pub id: u64,
    #[serde(default)]
    pub body: String,
    pub author: Author,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub system: bool,
    #[serde(default)]
    pub resolvable: bool,
    #[serde(default)]
    pub resolved: bool,
    #[serde(default)]
    pub position: Option<Position>,
}

/// A thread of notes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Discussion {
    pub id: String,
    #[serde(default)]
    pub individual_note: bool,
    pub notes: Vec<Note>,
}

impl Discussion {
    /// Only system notes ("added 2 commits"...).
    pub fn is_system(&self) -> bool {
        self.notes.iter().all(|n| n.system)
    }

    /// Resolvable and resolved.
    pub fn is_resolved(&self) -> bool {
        let mut r = self.notes.iter().filter(|n| n.resolvable).peekable();
        r.peek().is_some() && r.all(|n| n.resolved)
    }

    pub fn is_resolvable(&self) -> bool {
        self.notes.iter().any(|n| n.resolvable)
    }

    /// Position of the first note (diff discussions).
    pub fn position(&self) -> Option<&Position> {
        self.notes.first().and_then(|n| n.position.as_ref())
    }
}

/// A pending draft note of the current user.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftNote {
    pub id: u64,
    #[serde(default)]
    pub note: String,
    #[serde(default)]
    pub position: Option<Position>,
}

/// JSON body of `POST .../draft_notes`.
pub fn draft_note_payload(c: &NewComment) -> serde_json::Value {
    let mut v = serde_json::json!({ "note": c.body });
    if let Some(p) = &c.position {
        v["position"] = serde_json::to_value(p).expect("position serializes");
    }
    if let Some(d) = &c.in_reply_to {
        v["in_reply_to_discussion_id"] = d.clone().into();
    }
    v
}

/// Path (under the merge request) and JSON body of a comment posted right
/// away: `discussions`, or `discussions/<id>/notes` for a reply.
pub fn post_comment_request(c: &NewComment) -> (String, serde_json::Value) {
    let mut v = serde_json::json!({ "body": c.body });
    if let Some(p) = &c.position {
        v["position"] = serde_json::to_value(p).expect("position serializes");
    }
    match &c.in_reply_to {
        Some(d) => (format!("/discussions/{d}/notes"), v),
        None => ("/discussions".to_string(), v),
    }
}

/// What the instance supports for publishing a review, from its version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capabilities {
    pub version: String,
    /// Draft notes API with `bulk_publish`: the review is published at once.
    pub draft_notes: bool,
    /// `position_type: file` (comment on a whole file).
    pub file_comments: bool,
}

/// Draft notes API (create + bulk publish).
pub const DRAFT_NOTES_SINCE: (u32, u32) = (15, 10);
/// Comments on a whole file (`position_type: file`).
pub const FILE_COMMENTS_SINCE: (u32, u32) = (16, 4);

impl Capabilities {
    /// From `GET /version` (`17.5.1-ee`). An unreadable version is assumed recent.
    pub fn from_version(version: &str) -> Self {
        let mut parts = version.trim().split(['.', '-']);
        let major = parts.next().and_then(|p| p.parse::<u32>().ok());
        let minor = parts
            .next()
            .and_then(|p| p.parse::<u32>().ok())
            .unwrap_or(0);
        let at_least = |(a, b): (u32, u32)| major.is_none_or(|m| (m, minor) >= (a, b));
        Self {
            version: version.to_string(),
            draft_notes: at_least(DRAFT_NOTES_SINCE),
            file_comments: at_least(FILE_COMMENTS_SINCE),
        }
    }
}

/// What the user asked to review: `123`, `!123`, or a merge request URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MrRef {
    /// Project path when given by a URL.
    pub project: Option<String>,
    pub iid: u64,
}

impl MrRef {
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        if let Ok(iid) = s.trim_start_matches('!').parse() {
            return Some(Self { project: None, iid });
        }
        let url = url::Url::parse(s).ok()?;
        let path = url.path().trim_matches('/');
        let (project, rest) = path.split_once("/-/merge_requests/")?;
        let iid = rest.split(['/', '#', '?']).next()?.parse().ok()?;
        Some(Self {
            project: Some(project.to_string()),
            iid,
        })
    }
}

/// Project path (`group/sub/project`) from a git remote URL, SSH or HTTP(S).
pub fn project_from_remote(remote: &str) -> Option<String> {
    let remote = remote.trim();
    let path = if let Ok(url) = url::Url::parse(remote)
        && url.has_host()
    {
        url.path().to_string()
    } else {
        // scp-like: git@host:group/project.git
        remote.split_once(':')?.1.to_string()
    };
    let path = path.trim_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    (!path.is_empty() && path.contains('/')).then(|| path.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_mr_refs() {
        assert_eq!(
            MrRef::parse("42"),
            Some(MrRef {
                project: None,
                iid: 42
            })
        );
        assert_eq!(MrRef::parse("!7").unwrap().iid, 7);
        assert_eq!(
            MrRef::parse("https://gitlab.corp/a/b/c/-/merge_requests/12/diffs#note_1"),
            Some(MrRef {
                project: Some("a/b/c".into()),
                iid: 12
            })
        );
        assert_eq!(MrRef::parse("feature/x"), None);
    }

    #[test]
    fn capabilities_follow_the_version() {
        let c = Capabilities::from_version("17.5.1-ee");
        assert!(c.draft_notes && c.file_comments);
        let c = Capabilities::from_version("16.3.0");
        assert!(c.draft_notes && !c.file_comments);
        let c = Capabilities::from_version("15.9.4-ee");
        assert!(!c.draft_notes && !c.file_comments);
        assert!(Capabilities::from_version("unknown").draft_notes);
    }

    #[test]
    fn parses_remotes() {
        for r in [
            "git@gitlab.corp:team/sub/app.git",
            "ssh://git@gitlab.corp:2222/team/sub/app.git",
            "https://gitlab.corp/team/sub/app.git",
            "https://oauth2:tok@gitlab.corp/team/sub/app",
        ] {
            assert_eq!(
                project_from_remote(r).as_deref(),
                Some("team/sub/app"),
                "{r}"
            );
        }
    }
}
