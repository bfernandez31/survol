//! Forge abstraction: GitLab (self-hosted) and GitHub (github.com or
//! Enterprise Server), chosen from the git remote, the config or the URL
//! given on the command line (see [`detect`]).

#[cfg(test)]
pub(crate) mod fake;
pub mod github;
pub mod gitlab;

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::model::MergeRequest;

#[derive(Debug, thiserror::Error)]
pub enum ForgeError {
    #[error("no GitLab host configured: set GITLAB_HOST or [gitlab] host in the config")]
    NoHost,
    #[error("no token for {0}: log in with `glab auth login --hostname {0}` or set GITLAB_TOKEN")]
    NoToken(String),
    #[error(
        "no GitHub token for {0}: log in with `gh auth login --hostname {0}` or set GITHUB_TOKEN"
    )]
    NoGithubToken(String),
    #[error("GitHub GraphQL: {0}")]
    Graphql(String),
    #[error("no pending review to submit on this pull request")]
    NoPendingReview,
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
    #[error("no open merge / pull request for branch `{0}`")]
    NoMergeRequestForBranch(String),
    #[error("cannot determine the project: {0}")]
    Project(String),
}

pub type Result<T> = std::result::Result<T, ForgeError>;

/// Which kind of forge hosts the merge request.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ForgeKind {
    #[default]
    Gitlab,
    Github,
}

impl ForgeKind {
    /// `GitLab` / `GitHub`, for messages.
    pub fn name(self) -> &'static str {
        match self {
            Self::Gitlab => "GitLab",
            Self::Github => "GitHub",
        }
    }

    /// `MR` / `PR`.
    pub fn abbrev(self) -> &'static str {
        match self {
            Self::Gitlab => "MR",
            Self::Github => "PR",
        }
    }

    /// `!` / `#`: how the forge writes a merge request number.
    pub fn sigil(self) -> char {
        match self {
            Self::Gitlab => '!',
            Self::Github => '#',
        }
    }

    /// The project as it appears in API paths: URL-encoded for GitLab,
    /// `owner/repo` as is for GitHub.
    pub fn api_project(self, project: &str) -> String {
        match self {
            Self::Gitlab => gitlab::encode(project),
            Self::Github => project.to_string(),
        }
    }
}

impl std::fmt::Display for ForgeKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

pub trait Forge {
    fn kind(&self) -> ForgeKind;

    /// Human-readable server version, e.g. `17.5.1-ee`.
    fn server_version(&self) -> Result<String>;

    /// What the server supports for publishing a review.
    fn capabilities(&self) -> Result<Capabilities> {
        Ok(Capabilities::from_version(&self.server_version()?))
    }

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
    #[serde(default)]
    pub forge: ForgeKind,
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
            forge: ForgeKind::Gitlab,
            version: version.to_string(),
            draft_notes: at_least(DRAFT_NOTES_SINCE),
            file_comments: at_least(FILE_COMMENTS_SINCE),
        }
    }

    /// GitHub: a pending review, submitted at once, and file comments,
    /// whatever the version.
    pub fn github(version: &str) -> Self {
        Self {
            forge: ForgeKind::Github,
            version: version.to_string(),
            draft_notes: true,
            file_comments: true,
        }
    }
}

/// What the user asked to review: `123`, `!123`, `#123`, or a merge /
/// pull request URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MrRef {
    /// Project path when given by a URL.
    pub project: Option<String>,
    pub iid: u64,
    /// Forge and host when given by a URL.
    pub forge: Option<(ForgeKind, String)>,
}

impl MrRef {
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        if let Ok(iid) = s.trim_start_matches(['!', '#']).parse() {
            return Some(Self {
                project: None,
                iid,
                forge: None,
            });
        }
        let url = url::Url::parse(s).ok()?;
        let host = match url.port() {
            Some(port) => format!("{}:{port}", url.host_str()?),
            None => url.host_str()?.to_string(),
        };
        let path = url.path().trim_matches('/');
        let number = |rest: &str| rest.split(['/', '#', '?']).next()?.parse().ok();
        if let Some((project, rest)) = path.split_once("/-/merge_requests/") {
            return Some(Self {
                project: Some(project.to_string()),
                iid: number(rest)?,
                forge: Some((ForgeKind::Gitlab, host)),
            });
        }
        // GitHub: /owner/repo/pull/12[/files]
        let mut parts = path.splitn(4, '/');
        let (owner, repo, pull, rest) =
            (parts.next()?, parts.next()?, parts.next()?, parts.next()?);
        (pull == "pull").then_some(())?;
        Some(Self {
            project: Some(format!("{owner}/{repo}")),
            iid: number(rest)?,
            forge: Some((ForgeKind::Github, host)),
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

/// Host of a git remote URL, SSH or HTTP(S), without user nor port.
pub fn host_from_remote(remote: &str) -> Option<String> {
    let remote = remote.trim();
    if let Ok(url) = url::Url::parse(remote)
        && url.has_host()
    {
        return url.host_str().map(str::to_ascii_lowercase);
    }
    // scp-like: git@host:group/project.git
    let (user_host, _) = remote.split_once(':')?;
    let host = user_host.rsplit_once('@').map_or(user_host, |(_, h)| h);
    (!host.is_empty() && !host.contains('/')).then(|| host.to_ascii_lowercase())
}

/// The forge to talk to, and its host (`None`: GitLab's configured host).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForgeTarget {
    pub kind: ForgeKind,
    pub host: Option<String>,
    /// How it was chosen, for `doctor`.
    pub reason: String,
}

/// `github.com`, or an SSH alias of it such as `github.com-work`.
fn is_github_dot_com(host: &str) -> bool {
    let host = host.trim().to_ascii_lowercase();
    host == "www.github.com" || host == "ssh.github.com" || host.starts_with("github.com")
}

/// A GitHub host: github.com, or a GitHub Enterprise host listed in
/// `[github] hosts`.
pub fn is_github_host(cfg: &Config, host: &str) -> bool {
    let bare = gitlab::bare_host(host).to_ascii_lowercase();
    is_github_dot_com(&bare)
        || cfg
            .github
            .hosts
            .iter()
            .any(|h| gitlab::bare_host(h).eq_ignore_ascii_case(&bare))
}

/// Chooses the forge: a merge / pull request URL wins, then `[forge] kind`,
/// then the host of the git remote (github.com or a `[github] hosts` entry:
/// GitHub; anything else: GitLab, as before GitHub support).
pub fn detect(cfg: &Config, remote_url: Option<&str>, mr: Option<&MrRef>) -> ForgeTarget {
    let remote_host = remote_url.and_then(host_from_remote);
    // `[forge] host`, else `host`; an SSH alias of github.com is github.com.
    let github_host = |host: Option<String>| {
        Some(match cfg.forge.host.clone().or(host) {
            Some(h) if is_github_dot_com(&gitlab::bare_host(&h)) => "github.com".to_string(),
            Some(h) => h,
            None => "github.com".to_string(),
        })
    };
    let gitlab_host = |url_host: Option<String>| {
        cfg.gitlab
            .host
            .clone()
            .or(cfg.forge.host.clone())
            .or(url_host)
    };
    if let Some(MrRef {
        forge: Some((kind, host)),
        ..
    }) = mr
    {
        return match kind {
            ForgeKind::Github => ForgeTarget {
                kind: *kind,
                host: Some(if is_github_dot_com(host) {
                    "github.com".into()
                } else {
                    host.clone()
                }),
                reason: "from the URL".into(),
            },
            ForgeKind::Gitlab => ForgeTarget {
                kind: *kind,
                host: gitlab_host(Some(host.clone())),
                reason: "from the URL".into(),
            },
        };
    }
    if let Some(kind) = cfg.forge.kind {
        let host = match kind {
            ForgeKind::Github => github_host(remote_host),
            ForgeKind::Gitlab => gitlab_host(None),
        };
        return ForgeTarget {
            kind,
            host,
            reason: "[forge] kind in the config".into(),
        };
    }
    match remote_host {
        Some(h) if is_github_host(cfg, &h) => ForgeTarget {
            kind: ForgeKind::Github,
            host: github_host(Some(h.clone())),
            reason: format!("remote host {h}"),
        },
        _ => ForgeTarget {
            kind: ForgeKind::Gitlab,
            host: gitlab_host(None),
            reason: match remote_host {
                Some(h) => format!("remote host {h} is not a GitHub host"),
                None => "no git remote: GitLab by default".into(),
            },
        },
    }
}

/// A client for `target`.
pub fn connect(cfg: &Config, target: &ForgeTarget) -> Result<Box<dyn Forge + Send>> {
    Ok(match target.kind {
        ForgeKind::Gitlab => {
            let mut gl = cfg.gitlab.clone();
            gl.host = target.host.clone().or(gl.host);
            Box::new(gitlab::Gitlab::from_config(&gl)?)
        }
        ForgeKind::Github => {
            let host = target.host.as_deref().unwrap_or("github.com");
            Box::new(github::Github::from_config(host, &cfg.github)?)
        }
    })
}

/// A blocking HTTP client trusting `ca_cert` on top of the system store.
pub(crate) fn http_client(ca_cert: Option<&std::path::Path>) -> Result<reqwest::blocking::Client> {
    let mut builder = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(60))
        .user_agent(concat!("survol/", env!("CARGO_PKG_VERSION")));
    if let Some(path) = ca_cert {
        let ca_err = |message: String| ForgeError::CaCert {
            path: path.display().to_string(),
            message,
        };
        let pem = std::fs::read(path).map_err(|e| ca_err(e.to_string()))?;
        let certs =
            reqwest::Certificate::from_pem_bundle(&pem).map_err(|e| ca_err(e.to_string()))?;
        builder = builder.tls_certs_merge(certs);
    }
    Ok(builder.build()?)
}

/// The response when successful; otherwise its status, URL and the start
/// of its body (never the request's headers: they carry the token).
pub(crate) fn check_status(
    resp: reqwest::blocking::Response,
    url: String,
) -> Result<reqwest::blocking::Response> {
    let status = resp.status();
    if status.is_success() {
        return Ok(resp);
    }
    let body: String = resp.text().unwrap_or_default().chars().take(300).collect();
    Err(ForgeError::Status {
        status: status.as_u16(),
        url,
        body,
    })
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
                iid: 42,
                forge: None,
            })
        );
        assert_eq!(MrRef::parse("!7").unwrap().iid, 7);
        assert_eq!(MrRef::parse("#8").unwrap().iid, 8);
        assert_eq!(
            MrRef::parse("https://gitlab.corp/a/b/c/-/merge_requests/12/diffs#note_1"),
            Some(MrRef {
                project: Some("a/b/c".into()),
                iid: 12,
                forge: Some((ForgeKind::Gitlab, "gitlab.corp".into())),
            })
        );
        assert_eq!(
            MrRef::parse("https://github.com/BurntSushi/ripgrep/pull/3529/files#diff-1"),
            Some(MrRef {
                project: Some("BurntSushi/ripgrep".into()),
                iid: 3529,
                forge: Some((ForgeKind::Github, "github.com".into())),
            })
        );
        assert_eq!(
            MrRef::parse("https://ghe.corp:8443/team/app/pull/5")
                .unwrap()
                .forge,
            Some((ForgeKind::Github, "ghe.corp:8443".into()))
        );
        assert_eq!(MrRef::parse("https://github.com/o/r/issues/5"), None);
        assert_eq!(MrRef::parse("https://github.com/o/r/pull/x"), None);
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
            assert_eq!(host_from_remote(r).as_deref(), Some("gitlab.corp"), "{r}");
        }
        for r in [
            "git@github.com:BurntSushi/ripgrep.git",
            "ssh://git@github.com/BurntSushi/ripgrep.git",
            "https://github.com/BurntSushi/ripgrep",
            "https://x-access-token:tok@github.com/BurntSushi/ripgrep.git",
        ] {
            assert_eq!(
                project_from_remote(r).as_deref(),
                Some("BurntSushi/ripgrep"),
                "{r}"
            );
            assert_eq!(host_from_remote(r).as_deref(), Some("github.com"), "{r}");
        }
        assert_eq!(
            host_from_remote("git@GitHub.com-work:o/r.git").as_deref(),
            Some("github.com-work")
        );
        assert_eq!(host_from_remote("/srv/git/app.git"), None);
    }

    fn cfg(kind: Option<ForgeKind>, github_hosts: &[&str]) -> Config {
        let mut cfg = Config::default();
        cfg.forge.kind = kind;
        cfg.github.hosts = github_hosts.iter().map(|h| h.to_string()).collect();
        cfg.gitlab.host = Some("gitlab.corp".into());
        cfg
    }

    #[test]
    fn detects_the_forge() {
        let target = |cfg: &Config, remote: Option<&str>, arg: Option<&str>| {
            let mr = arg.and_then(MrRef::parse);
            let t = detect(cfg, remote, mr.as_ref());
            (t.kind, t.host)
        };
        let gh = |h: &str| (ForgeKind::Github, Some(h.to_string()));
        let gl = |h: &str| (ForgeKind::Gitlab, Some(h.to_string()));
        let plain = cfg(None, &["ghe.corp"]);

        // From the remote: github.com (and its SSH aliases), listed
        // Enterprise hosts; anything else is GitLab with its configured host.
        assert_eq!(
            target(&plain, Some("git@github.com:o/r.git"), None),
            gh("github.com")
        );
        assert_eq!(
            target(&plain, Some("git@github.com-work:o/r.git"), Some("12")),
            gh("github.com")
        );
        assert_eq!(
            target(&plain, Some("https://GHE.corp/o/r.git"), None),
            gh("ghe.corp")
        );
        assert_eq!(
            target(&plain, Some("git@gitlab.corp:g/app.git"), None),
            gl("gitlab.corp")
        );
        assert_eq!(
            target(&plain, Some("git@other.example:g/app.git"), None),
            gl("gitlab.corp")
        );
        assert_eq!(target(&plain, None, None), gl("gitlab.corp"));
        let mut no_host = Config::default();
        no_host.gitlab.host = None;
        assert_eq!(
            target(&no_host, Some("git@gitlab.corp:g/app.git"), None),
            (ForgeKind::Gitlab, None)
        );

        // A URL wins over the remote.
        assert_eq!(
            target(
                &plain,
                Some("git@gitlab.corp:g/app.git"),
                Some("https://github.com/o/r/pull/3")
            ),
            gh("github.com")
        );
        assert_eq!(
            target(
                &plain,
                Some("git@github.com:o/r.git"),
                Some("https://gitlab.corp/g/app/-/merge_requests/3")
            ),
            gl("gitlab.corp")
        );
        assert_eq!(
            target(
                &no_host,
                None,
                Some("https://gl.example/g/app/-/merge_requests/3")
            ),
            gl("gl.example")
        );

        // `[forge] kind` wins over the remote; `[forge] host` names the host.
        let forced = cfg(Some(ForgeKind::Github), &[]);
        assert_eq!(
            target(&forced, Some("git@ghe-alias:o/r.git"), None),
            gh("ghe-alias")
        );
        let mut forced_host = forced.clone();
        forced_host.forge.host = Some("github.corp.example".into());
        assert_eq!(
            target(&forced_host, Some("git@ghe-alias:o/r.git"), None),
            gh("github.corp.example")
        );
        assert_eq!(
            target(&forced_host, None, Some("https://ghe.other/o/r/pull/1")),
            gh("ghe.other")
        );
        let gitlab = cfg(Some(ForgeKind::Gitlab), &[]);
        assert_eq!(
            target(&gitlab, Some("git@github.com:o/r.git"), None),
            gl("gitlab.corp")
        );
    }

    #[test]
    fn forge_wording() {
        assert_eq!(ForgeKind::Github.to_string(), "GitHub");
        assert_eq!(ForgeKind::Gitlab.abbrev(), "MR");
        assert_eq!(ForgeKind::Github.api_project("o/r"), "o/r");
        assert_eq!(ForgeKind::Gitlab.api_project("g/app"), "g%2Fapp");
        let c = Capabilities::github("github.com");
        assert!(c.draft_notes && c.file_comments);
        assert_eq!(Capabilities::from_version("17.0").forge, ForgeKind::Gitlab);
    }
}
