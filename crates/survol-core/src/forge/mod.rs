//! Forge abstraction. GitLab (self-hosted) first; GitHub may come later.

pub mod gitlab;

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
