//! GitLab REST API v4 client for a self-hosted instance.

use std::process::Command;
use std::time::Duration;

use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use reqwest::blocking::{Client, Response};
use serde::Deserialize;
use serde::de::DeserializeOwned;

use super::{Forge, ForgeError, Result};
use crate::config::GitlabConfig;
use crate::model::MergeRequest;

pub struct Gitlab {
    client: Client,
    api: String,
    token: String,
}

impl Gitlab {
    /// Builds a client from the config: host, CA, and a token from
    /// `GITLAB_TOKEN` or glab's configuration for that host.
    pub fn from_config(cfg: &GitlabConfig) -> Result<Self> {
        let host = cfg.host.as_deref().ok_or(ForgeError::NoHost)?;
        let token = find_token(host).ok_or_else(|| ForgeError::NoToken(bare_host(host)))?;
        Self::new(host, token, cfg.ca_cert.as_deref())
    }

    pub fn new(host: &str, token: String, ca_cert: Option<&std::path::Path>) -> Result<Self> {
        let mut builder = Client::builder()
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
        Ok(Self {
            client: builder.build()?,
            api: format!("{}/api/v4", base_url(host)),
            token,
        })
    }

    fn get(&self, path: &str, query: &[(&str, &str)]) -> Result<Response> {
        let url = format!("{}{}", self.api, path);
        let resp = self
            .client
            .get(&url)
            .bearer_auth(&self.token)
            .query(query)
            .send()?;
        let status = resp.status();
        if status.is_success() {
            return Ok(resp);
        }
        let mut body = resp.text().unwrap_or_default();
        body.truncate(300);
        Err(ForgeError::Status {
            status: status.as_u16(),
            url,
            body,
        })
    }

    fn get_json<T: DeserializeOwned>(&self, path: &str, query: &[(&str, &str)]) -> Result<T> {
        Ok(self.get(path, query)?.json()?)
    }
}

fn encode(project: &str) -> String {
    utf8_percent_encode(project, NON_ALPHANUMERIC).to_string()
}

#[derive(Deserialize)]
struct ApiMergeRequest {
    iid: u64,
    title: String,
    #[serde(default)]
    description: Option<String>,
    source_branch: String,
    target_branch: String,
    web_url: String,
    sha: Option<String>,
    diff_refs: Option<ApiDiffRefs>,
}

#[derive(Deserialize)]
struct ApiDiffRefs {
    base_sha: Option<String>,
    start_sha: Option<String>,
    head_sha: Option<String>,
}

#[derive(Deserialize)]
struct ApiVersion {
    version: String,
}

#[derive(Deserialize)]
struct ApiMrVersion {
    base_commit_sha: String,
    start_commit_sha: String,
    head_commit_sha: String,
}

impl Forge for Gitlab {
    fn server_version(&self) -> Result<String> {
        Ok(self.get_json::<ApiVersion>("/version", &[])?.version)
    }

    fn merge_request(&self, project: &str, iid: u64) -> Result<MergeRequest> {
        let p = encode(project);
        let mr: ApiMergeRequest =
            self.get_json(&format!("/projects/{p}/merge_requests/{iid}"), &[])?;
        let refs = mr.diff_refs.as_ref();
        let (mut base, mut start, mut head) = (
            refs.and_then(|r| r.base_sha.clone()),
            refs.and_then(|r| r.start_sha.clone()),
            refs.and_then(|r| r.head_sha.clone()).or(mr.sha.clone()),
        );
        // diff_refs can be missing while GitLab computes the diff: fall back
        // on the latest MR version.
        if base.is_none() || start.is_none() || head.is_none() {
            let versions: Vec<ApiMrVersion> =
                self.get_json(&format!("/projects/{p}/merge_requests/{iid}/versions"), &[])?;
            if let Some(v) = versions.into_iter().next() {
                base = Some(v.base_commit_sha);
                start = Some(v.start_commit_sha);
                head = Some(v.head_commit_sha);
            }
        }
        let missing = || ForgeError::Project(format!("merge request !{iid} has no diff yet"));
        Ok(MergeRequest {
            project: project.to_string(),
            iid: mr.iid,
            title: mr.title,
            description: mr.description.unwrap_or_default(),
            source_branch: mr.source_branch,
            target_branch: mr.target_branch,
            base_sha: base.ok_or_else(missing)?,
            start_sha: start.ok_or_else(missing)?,
            head_sha: head.ok_or_else(missing)?,
            web_url: mr.web_url,
        })
    }

    fn merge_request_for_branch(&self, project: &str, branch: &str) -> Result<u64> {
        let mrs: Vec<ApiMergeRequest> = self.get_json(
            &format!("/projects/{}/merge_requests", encode(project)),
            &[("source_branch", branch), ("state", "opened")],
        )?;
        mrs.first()
            .map(|m| m.iid)
            .ok_or_else(|| ForgeError::NoMergeRequestForBranch(branch.to_string()))
    }
}

/// `https://host` from `host`, `host:port` or a full URL.
pub fn base_url(host: &str) -> String {
    let host = host.trim().trim_end_matches('/');
    if host.starts_with("http://") || host.starts_with("https://") {
        host.to_string()
    } else {
        format!("https://{host}")
    }
}

/// Host name without scheme nor trailing slash, as glab stores it.
pub fn bare_host(host: &str) -> String {
    let h = host.trim().trim_end_matches('/');
    h.split_once("://").map_or(h, |(_, rest)| rest).to_string()
}

/// Token lookup: `GITLAB_TOKEN`, then glab's config for this host.
pub fn find_token(host: &str) -> Option<String> {
    if let Ok(t) = std::env::var("GITLAB_TOKEN")
        && !t.trim().is_empty()
    {
        return Some(t.trim().to_string());
    }
    glab_token(&bare_host(host))
}

/// Where the token came from, for `doctor`.
pub fn token_source(host: &str) -> Option<&'static str> {
    if std::env::var("GITLAB_TOKEN").is_ok_and(|t| !t.trim().is_empty()) {
        Some("GITLAB_TOKEN")
    } else if glab_token(&bare_host(host)).is_some() {
        Some("glab")
    } else {
        None
    }
}

fn glab_token(host: &str) -> Option<String> {
    let out = Command::new("glab")
        .args(["config", "get", "token", "--host", host])
        .output()
        .ok()?;
    let t = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (out.status.success() && !t.is_empty()).then_some(t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_normalisation() {
        assert_eq!(base_url("gitlab.corp"), "https://gitlab.corp");
        assert_eq!(
            base_url("http://gitlab.local:8080/"),
            "http://gitlab.local:8080"
        );
        assert_eq!(bare_host("https://gitlab.corp/"), "gitlab.corp");
        assert_eq!(encode("a/b-c.d"), "a%2Fb%2Dc%2Ed");
    }
}

#[cfg(test)]
mod http_tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;

    /// Serves one canned JSON response per expected request, and reports the
    /// request lines and authorization headers it received.
    fn serve(responses: Vec<(&'static str, String)>) -> (String, mpsc::Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = format!("http://{}", listener.local_addr().unwrap());
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            for (status, body) in responses {
                let (mut stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut request = String::new();
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" || line.is_empty() {
                        break;
                    }
                    let lower = line.to_ascii_lowercase();
                    if request.is_empty() || lower.starts_with("authorization") {
                        request.push_str(line.trim_end());
                        request.push('|');
                    }
                }
                tx.send(request).unwrap();
                write!(
                    stream,
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            }
        });
        (addr, rx)
    }

    #[test]
    fn fetches_merge_request_and_falls_back_on_versions() {
        let mr = r#"{"iid":7,"title":"Orders","description":null,"source_branch":"feat",
            "target_branch":"main","web_url":"https://g/x/-/merge_requests/7","sha":"h",
            "diff_refs":null}"#;
        let versions = r#"[{"base_commit_sha":"b","start_commit_sha":"s","head_commit_sha":"h"},
            {"base_commit_sha":"old","start_commit_sha":"old","head_commit_sha":"old"}]"#;
        let (addr, rx) = serve(vec![("200 OK", mr.into()), ("200 OK", versions.into())]);

        let gl = Gitlab::new(&addr, "tok".into(), None).unwrap();
        let got = gl.merge_request("grp/sub/app", 7).unwrap();
        assert_eq!(
            (
                got.base_sha.as_str(),
                got.start_sha.as_str(),
                got.head_sha.as_str()
            ),
            ("b", "s", "h")
        );
        assert_eq!(got.project, "grp/sub/app");
        assert_eq!(got.description, "");

        let first = rx.recv().unwrap();
        assert!(
            first.starts_with("GET /api/v4/projects/grp%2Fsub%2Fapp/merge_requests/7 "),
            "{first}"
        );
        assert!(first.contains("Bearer tok"), "{first}");
        assert!(rx.recv().unwrap().contains("/merge_requests/7/versions"));
    }

    #[test]
    fn reports_http_errors() {
        let (addr, _rx) = serve(vec![(
            "404 Not Found",
            r#"{"message":"404 Project Not Found"}"#.into(),
        )]);
        let gl = Gitlab::new(&addr, "tok".into(), None).unwrap();
        match gl.merge_request_for_branch("a/b", "feat") {
            Err(ForgeError::Status {
                status: 404, body, ..
            }) => assert!(body.contains("Project Not Found")),
            other => panic!("unexpected {other:?}"),
        }
    }
}
