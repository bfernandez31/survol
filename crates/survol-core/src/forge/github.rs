//! GitHub client (github.com or GitHub Enterprise Server): REST v3 for pull
//! requests and comments, GraphQL for review threads (resolved state) and
//! for the pending review a publication fills before submitting it.
//!
//! Survol's review maps onto one GitHub review: [`Forge::create_draft_note`]
//! adds a thread, a reply or a paragraph of the review body to the current
//! user's pending review (created on the first call), and
//! [`Forge::publish_drafts`] submits it with the `COMMENT` event.

use std::process::Command;
use std::sync::Mutex;

use reqwest::blocking::{Client, RequestBuilder, Response};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use super::gitlab::bare_host;
use super::{
    Author, Capabilities, Discussion, DraftNote, Forge, ForgeError, ForgeKind, NewComment, Note,
    Position, Result, check_status, http_client,
};
use crate::config::GithubConfig;
use crate::model::MergeRequest;

/// Prefix of the ids of general discussions (PR conversation comments and
/// review bodies): GitHub cannot thread replies to them, so a reply becomes
/// a paragraph of the review body mentioning the author.
pub const GENERAL: &str = "general:";

/// A discussion that is not a review thread (see [`GENERAL`]).
pub fn is_general(discussion: &str) -> bool {
    discussion.starts_with(GENERAL)
}

pub struct Github {
    client: Client,
    host: String,
    api: String,
    graphql: String,
    token: String,
    /// The current user's pending review, once found or created.
    pending: Mutex<Option<Pending>>,
}

#[derive(Debug, Clone)]
struct Pending {
    project: String,
    number: u64,
    node_id: String,
    id: u64,
    body: String,
}

impl Github {
    /// Builds a client for `host` (`github.com` or an Enterprise Server
    /// host), with a token from [`find_token`].
    pub fn from_config(host: &str, cfg: &GithubConfig) -> Result<Self> {
        let bare = bare_host(host);
        let (token, _) = find_token(&bare, cfg.token.as_deref())
            .ok_or_else(|| ForgeError::NoGithubToken(bare.clone()))?;
        let (api, graphql) = api_urls(host);
        Self::new(&bare, &api, &graphql, token, cfg.ca_cert.as_deref())
    }

    pub fn new(
        host: &str,
        api: &str,
        graphql: &str,
        token: String,
        ca_cert: Option<&std::path::Path>,
    ) -> Result<Self> {
        Ok(Self {
            client: http_client(ca_cert)?,
            host: host.to_string(),
            api: api.trim_end_matches('/').to_string(),
            graphql: graphql.to_string(),
            token,
            pending: Mutex::new(None),
        })
    }

    fn send(&self, req: RequestBuilder, url: String) -> Result<Response> {
        let req = req
            .bearer_auth(&self.token)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28");
        check_status(req.send()?, url)
    }

    fn get(&self, path: &str, query: &[(&str, &str)]) -> Result<Response> {
        let url = format!("{}{}", self.api, path);
        let req = self.client.get(&url).query(query);
        self.send(req, url)
    }

    fn get_json<T: DeserializeOwned>(&self, path: &str, query: &[(&str, &str)]) -> Result<T> {
        Ok(self.get(path, query)?.json()?)
    }

    fn post<T: DeserializeOwned>(&self, path: &str, body: &Value) -> Result<T> {
        let url = format!("{}{}", self.api, path);
        let req = self.client.post(&url).json(body);
        Ok(self.send(req, url)?.json()?)
    }

    /// Every page of a list endpoint, following the `Link: rel="next"` header.
    fn get_all<T: DeserializeOwned>(&self, path: &str) -> Result<Vec<T>> {
        let mut out = Vec::new();
        let mut resp = self.get(path, &[("per_page", "100")])?;
        for _ in 0..MAX_PAGES {
            let next = resp
                .headers()
                .get("link")
                .and_then(|v| v.to_str().ok())
                .and_then(next_link);
            out.extend(resp.json::<Vec<T>>()?);
            let Some(url) = next else {
                break;
            };
            let req = self.client.get(&url);
            resp = self.send(req, url)?;
        }
        Ok(out)
    }

    /// A GraphQL query; its `errors` become [`ForgeError::Graphql`].
    fn graphql<T: DeserializeOwned>(&self, query: &str, variables: Value) -> Result<T> {
        let req = self
            .client
            .post(&self.graphql)
            .json(&json!({ "query": query, "variables": variables }));
        let resp: GqlResponse<T> = self.send(req, self.graphql.clone())?.json()?;
        if let Some(errors) = resp.errors.filter(|e| !e.is_empty()) {
            let msgs: Vec<String> = errors.into_iter().map(|e| e.message).collect();
            return Err(ForgeError::Graphql(msgs.join("; ")));
        }
        resp.data
            .ok_or_else(|| ForgeError::Graphql("empty response".into()))
    }

    /// Login of the token's user, for `doctor`.
    pub fn whoami(&self) -> Result<String> {
        Ok(self.get_json::<ApiUser>("/user", &[])?.login)
    }

    fn review_threads(&self, project: &str, number: u64) -> Result<Vec<Discussion>> {
        let (owner, name) = split(project)?;
        let mut out = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..MAX_PAGES {
            let data: ThreadsData = self.graphql(
                THREADS_QUERY,
                json!({"owner": owner, "name": name, "number": number, "cursor": cursor}),
            )?;
            let threads = pull_request(data.repository)?.review_threads;
            out.extend(threads.nodes.into_iter().map(thread_discussion));
            match threads.page_info {
                PageInfo {
                    has_next_page: true,
                    end_cursor: Some(c),
                } => cursor = Some(c),
                _ => break,
            }
        }
        Ok(out)
    }

    /// The current user's pending review, with its comments.
    fn find_pending(&self, project: &str, number: u64) -> Result<Option<ApiPendingReview>> {
        let (owner, name) = split(project)?;
        let data: PendingData = self.graphql(
            PENDING_QUERY,
            json!({"owner": owner, "name": name, "number": number}),
        )?;
        Ok(pull_request(data.repository)?
            .reviews
            .nodes
            .into_iter()
            .next())
    }

    /// The pending review, found or created (on `commit` when given).
    fn ensure_pending(&self, project: &str, number: u64, commit: Option<&str>) -> Result<Pending> {
        let mut cache = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(p) = cache.as_ref()
            && p.project == project
            && p.number == number
        {
            return Ok(p.clone());
        }
        let pending = match self.find_pending(project, number)? {
            Some(r) => Pending {
                project: project.to_string(),
                number,
                node_id: r.id,
                id: r.database_id,
                body: r.body,
            },
            None => {
                let created: ApiReview = self.post(
                    &format!("/repos/{project}/pulls/{number}/reviews"),
                    &create_review_payload(commit),
                )?;
                Pending {
                    project: project.to_string(),
                    number,
                    node_id: created.node_id,
                    id: created.id,
                    body: created.body.unwrap_or_default(),
                }
            }
        };
        *cache = Some(pending.clone());
        Ok(pending)
    }

    fn set_pending_body(&self, body: &str) {
        let mut cache = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(p) = cache.as_mut() {
            p.body = body.to_string();
        }
    }
}

/// Safety stop for pagination (10 000 items).
const MAX_PAGES: usize = 100;

/// REST and GraphQL endpoints of `host`: `api.github.com` for github.com,
/// `<host>/api/v3` and `<host>/api/graphql` for Enterprise Server.
pub fn api_urls(host: &str) -> (String, String) {
    let bare = bare_host(host).to_ascii_lowercase();
    if bare == "github.com" || bare == "api.github.com" {
        return (
            "https://api.github.com".into(),
            "https://api.github.com/graphql".into(),
        );
    }
    let base = super::gitlab::base_url(host);
    (format!("{base}/api/v3"), format!("{base}/api/graphql"))
}

/// The `rel="next"` URL of a `Link` header.
pub fn next_link(header: &str) -> Option<String> {
    header.split(',').find_map(|part| {
        let (url, params) = part.split_once(';')?;
        params
            .split(';')
            .any(|p| p.trim() == r#"rel="next""#)
            .then(|| {
                url.trim()
                    .trim_start_matches('<')
                    .trim_end_matches('>')
                    .to_string()
            })
    })
}

fn split(project: &str) -> Result<(&str, &str)> {
    project
        .split_once('/')
        .filter(|(o, r)| !o.is_empty() && !r.is_empty() && !r.contains('/'))
        .ok_or_else(|| ForgeError::Project(format!("`{project}` is not owner/repo")))
}

fn pull_request<T>(repo: Option<GqlRepository<T>>) -> Result<T> {
    repo.and_then(|r| r.pull_request)
        .ok_or_else(|| ForgeError::Graphql("pull request not found".into()))
}

// ----- tokens ---------------------------------------------------------------

fn is_dot_com(host: &str) -> bool {
    matches!(
        bare_host(host).to_ascii_lowercase().as_str(),
        "github.com" | "api.github.com"
    )
}

/// Token lookup, as gh does: `GITHUB_TOKEN`, `GH_TOKEN` for github.com
/// (`GH_ENTERPRISE_TOKEN`, `GITHUB_ENTERPRISE_TOKEN` for Enterprise Server),
/// then `[github] token`, then `gh auth token --hostname <host>`. Returns
/// the token and where it came from; the token is never printed.
pub fn find_token(host: &str, configured: Option<&str>) -> Option<(String, &'static str)> {
    token_from(host, configured, |k| std::env::var(k).ok(), gh_token)
}

fn token_from(
    host: &str,
    configured: Option<&str>,
    env: impl Fn(&str) -> Option<String>,
    gh: impl Fn(&str) -> Option<String>,
) -> Option<(String, &'static str)> {
    let vars: &[&'static str] = if is_dot_com(host) {
        &["GITHUB_TOKEN", "GH_TOKEN"]
    } else {
        &["GH_ENTERPRISE_TOKEN", "GITHUB_ENTERPRISE_TOKEN"]
    };
    let nonempty = |t: String| Some(t.trim().to_string()).filter(|t| !t.is_empty());
    vars.iter()
        .find_map(|&k| env(k).and_then(nonempty).map(|t| (t, k)))
        .or_else(|| {
            configured
                .map(str::to_string)
                .and_then(nonempty)
                .map(|t| (t, "[github] token"))
        })
        .or_else(|| gh(&bare_host(host)).and_then(nonempty).map(|t| (t, "gh")))
}

fn gh_token(host: &str) -> Option<String> {
    let out = Command::new("gh")
        .args(["auth", "token", "--hostname", host])
        .output()
        .ok()?;
    let t = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (out.status.success() && !t.is_empty()).then_some(t)
}

// ----- payloads -------------------------------------------------------------

/// Where a comment goes, in GitHub's terms.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Anchor<'a> {
    path: &'a str,
    /// `None`: the whole file.
    line: Option<(u32, &'static str)>,
    /// First line of a range, when it differs from `line`.
    start: Option<(u32, &'static str)>,
}

/// `RIGHT` and the new line for an added or unchanged line, `LEFT` and the
/// old line for a removed one.
fn side_line(new_line: Option<u32>, old_line: Option<u32>) -> Option<(u32, &'static str)> {
    match (new_line, old_line) {
        (Some(n), _) => Some((n, "RIGHT")),
        (None, Some(o)) => Some((o, "LEFT")),
        (None, None) => None,
    }
}

fn anchor(p: &Position) -> Anchor<'_> {
    if p.position_type == "file" {
        return Anchor {
            path: &p.new_path,
            line: None,
            start: None,
        };
    }
    let line = side_line(p.new_line, p.old_line);
    // A range ending on a removed line starts on the left side too (an
    // unchanged line by its old number); GitHub has no right-to-left range,
    // so one starting on an added line becomes a single-line comment.
    let start = p
        .line_range
        .as_ref()
        .and_then(|r| match line {
            Some((_, "LEFT")) => r.start.old_line.map(|o| (o, "LEFT")),
            _ => side_line(r.start.new_line, r.start.old_line),
        })
        .filter(|s| Some(*s) != line);
    Anchor {
        path: &p.new_path,
        line,
        start,
    }
}

/// Body of `POST /repos/{o}/{r}/pulls/{n}/reviews` creating a pending review
/// (no `event`): on `commit` when given, else on the head of the PR.
pub fn create_review_payload(commit: Option<&str>) -> Value {
    match commit.filter(|c| !c.is_empty()) {
        Some(c) => json!({ "commit_id": c }),
        None => json!({}),
    }
}

/// Body of `POST /repos/{o}/{r}/pulls/{n}/comments` (a diff comment posted
/// right away): `line` / `side`, `start_line` / `start_side` for a range,
/// `subject_type: file` for a whole file.
pub fn rest_comment_payload(body: &str, p: &Position) -> Value {
    let a = anchor(p);
    let mut v = json!({ "body": body, "commit_id": p.head_sha, "path": a.path });
    match a.line {
        None => v["subject_type"] = "file".into(),
        Some((line, side)) => {
            v["line"] = line.into();
            v["side"] = side.into();
            if let Some((start, start_side)) = a.start {
                v["start_line"] = start.into();
                v["start_side"] = start_side.into();
            }
        }
    }
    v
}

/// Input of the GraphQL `addPullRequestReviewThread` mutation: a diff
/// comment of the pending review `review` (a node id).
pub fn thread_input(body: &str, p: &Position, review: &str) -> Value {
    let a = anchor(p);
    let mut v = json!({ "pullRequestReviewId": review, "body": body, "path": a.path });
    match a.line {
        None => v["subjectType"] = "FILE".into(),
        Some((line, side)) => {
            v["subjectType"] = "LINE".into();
            v["line"] = line.into();
            v["side"] = side.into();
            if let Some((start, start_side)) = a.start {
                v["startLine"] = start.into();
                v["startSide"] = start_side.into();
            }
        }
    }
    v
}

/// The GraphQL mutation adding `c` to the pending review `review` (node
/// id), and its input: a thread for a diff comment, a thread reply, or
/// `updatePullRequestReview` for text of the review body (the overall
/// comment, a reply to a general comment), whose input shows the text
/// added to the body.
pub fn draft_mutation(c: &NewComment, review: &str) -> (&'static str, Value) {
    match (&c.position, &c.in_reply_to) {
        (Some(p), _) => (
            "addPullRequestReviewThread",
            thread_input(&c.body, p, review),
        ),
        (None, Some(thread)) if !is_general(thread) => (
            "addPullRequestReviewThreadReply",
            json!({
                "pullRequestReviewId": review,
                "pullRequestReviewThreadId": thread,
                "body": c.body,
            }),
        ),
        (None, _) => (
            "updatePullRequestReview",
            json!({ "pullRequestReviewId": review, "body": c.body }),
        ),
    }
}

/// The review body once `c` (overall comment or reply to a general
/// comment) is added: the overall comment on top, replies after.
fn review_body(current: &str, c: &NewComment) -> String {
    let current = current.trim();
    let text = c.body.trim_end();
    match (current.is_empty(), c.in_reply_to.is_some()) {
        (true, _) => text.to_string(),
        (false, true) => format!("{current}\n\n{text}"),
        (false, false) => format!("{text}\n\n{current}"),
    }
}

/// Method, path (under the API root) and body of a comment posted right
/// away: a diff comment, a thread reply (GraphQL), or a PR conversation
/// comment.
pub fn direct_request(
    project: &str,
    number: impl std::fmt::Display,
    c: &NewComment,
) -> (String, Value) {
    match (&c.position, &c.in_reply_to) {
        (Some(p), _) => (
            format!("/repos/{project}/pulls/{number}/comments"),
            rest_comment_payload(&c.body, p),
        ),
        (None, Some(thread)) if !is_general(thread) => (
            "/graphql".into(),
            json!({
                "mutation": "addPullRequestReviewThreadReply",
                "input": {"pullRequestReviewThreadId": thread, "body": c.body},
            }),
        ),
        (None, _) => (
            format!("/repos/{project}/issues/{number}/comments"),
            json!({ "body": c.body }),
        ),
    }
}

// ----- responses ------------------------------------------------------------

#[derive(Deserialize)]
struct GqlResponse<T> {
    data: Option<T>,
    errors: Option<Vec<GqlError>>,
}

#[derive(Deserialize)]
struct GqlError {
    message: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GqlRepository<T> {
    pull_request: Option<T>,
}

#[derive(Deserialize)]
struct ThreadsData {
    repository: Option<GqlRepository<ThreadsPr>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ThreadsPr {
    review_threads: Connection<ApiThread>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Connection<T> {
    #[serde(default = "PageInfo::last")]
    page_info: PageInfo,
    nodes: Vec<T>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PageInfo {
    has_next_page: bool,
    end_cursor: Option<String>,
}

impl PageInfo {
    fn last() -> Self {
        Self {
            has_next_page: false,
            end_cursor: None,
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ApiThread {
    id: String,
    is_resolved: bool,
    path: String,
    line: Option<u32>,
    diff_side: Option<String>,
    subject_type: Option<String>,
    comments: Connection<ApiThreadComment>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ApiThreadComment {
    database_id: u64,
    #[serde(default)]
    body: String,
    #[serde(default)]
    created_at: String,
    author: Option<ApiActor>,
}

#[derive(Deserialize)]
struct ApiActor {
    login: String,
}

#[derive(Deserialize)]
struct PendingData {
    repository: Option<GqlRepository<PendingPr>>,
}

#[derive(Deserialize)]
struct PendingPr {
    reviews: Connection<ApiPendingReview>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ApiPendingReview {
    id: String,
    database_id: u64,
    #[serde(default)]
    body: String,
    comments: Connection<ApiPendingComment>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ApiPendingComment {
    database_id: u64,
    #[serde(default)]
    body: String,
}

#[derive(Deserialize)]
struct ApiUser {
    login: String,
}

#[derive(Deserialize)]
struct ApiPull {
    number: u64,
    title: String,
    body: Option<String>,
    head: ApiBranch,
    base: ApiBranch,
    html_url: String,
}

#[derive(Deserialize)]
struct ApiBranch {
    #[serde(rename = "ref")]
    ref_name: String,
    sha: String,
}

#[derive(Deserialize)]
struct ApiReview {
    id: u64,
    node_id: String,
    #[serde(default)]
    state: String,
    body: Option<String>,
    user: Option<ApiActor>,
    #[serde(default)]
    submitted_at: Option<String>,
}

#[derive(Deserialize)]
struct ApiIssueComment {
    id: u64,
    node_id: String,
    body: Option<String>,
    user: Option<ApiActor>,
    #[serde(default)]
    created_at: String,
}

#[derive(Deserialize)]
struct ApiMeta {
    installed_version: Option<String>,
}

#[derive(Deserialize)]
struct CreatedThread {
    #[serde(rename = "addPullRequestReviewThread")]
    add: CreatedThreadPayload,
}

#[derive(Deserialize)]
struct CreatedThreadPayload {
    thread: CreatedThreadNode,
}

#[derive(Deserialize)]
struct CreatedThreadNode {
    comments: Connection<ApiPendingComment>,
}

#[derive(Deserialize)]
struct CreatedReply {
    #[serde(rename = "addPullRequestReviewThreadReply")]
    add: CreatedReplyPayload,
}

#[derive(Deserialize)]
struct CreatedReplyPayload {
    comment: ApiPendingComment,
}

#[derive(Deserialize)]
struct UpdatedReview {
    #[serde(rename = "updatePullRequestReview")]
    update: UpdatedReviewPayload,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdatedReviewPayload {
    pull_request_review: ApiPendingComment,
}

fn author(a: Option<ApiActor>) -> Author {
    Author {
        // Deleted accounts come back as `null`: GitHub shows them as ghost.
        username: a.map_or_else(|| "ghost".to_string(), |a| a.login),
        name: String::new(),
    }
}

/// A review thread as a discussion: resolvable, positioned on its line
/// (`new_line` on the right side, `old_line` on the left) or its file;
/// no line when outdated.
fn thread_discussion(t: ApiThread) -> Discussion {
    let file = t.subject_type.as_deref() == Some("FILE");
    let left = t.diff_side.as_deref() == Some("LEFT");
    let line = t.line.filter(|_| !file);
    let position = Position {
        position_type: if file { "file" } else { "text" }.into(),
        base_sha: String::new(),
        start_sha: String::new(),
        head_sha: String::new(),
        old_path: t.path.clone(),
        new_path: t.path,
        old_line: line.filter(|_| left),
        new_line: line.filter(|_| !left),
        line_range: None,
    };
    let resolved = t.is_resolved;
    Discussion {
        id: t.id,
        individual_note: false,
        notes: t
            .comments
            .nodes
            .into_iter()
            .map(|c| Note {
                id: c.database_id,
                body: c.body,
                author: author(c.author),
                created_at: c.created_at,
                system: false,
                resolvable: true,
                resolved,
                position: Some(position.clone()),
            })
            .collect(),
    }
}

/// A PR conversation comment or a submitted review's body: a general,
/// unresolvable discussion (see [`GENERAL`]).
fn general_discussion(
    node_id: &str,
    id: u64,
    body: String,
    user: Option<ApiActor>,
    at: String,
) -> Discussion {
    Discussion {
        id: format!("{GENERAL}{node_id}"),
        individual_note: true,
        notes: vec![Note {
            id,
            body,
            author: author(user),
            created_at: at,
            system: false,
            resolvable: false,
            resolved: false,
            position: None,
        }],
    }
}

/// Review threads, conversation comments and review bodies, oldest first.
fn merge_discussions(
    threads: Vec<Discussion>,
    comments: Vec<ApiIssueComment>,
    reviews: Vec<ApiReview>,
) -> Vec<Discussion> {
    let mut all = threads;
    all.extend(comments.into_iter().map(|c| {
        general_discussion(
            &c.node_id,
            c.id,
            c.body.unwrap_or_default(),
            c.user,
            c.created_at,
        )
    }));
    all.extend(
        reviews
            .into_iter()
            .filter(|r| r.state != "PENDING")
            .filter(|r| r.body.as_deref().is_some_and(|b| !b.trim().is_empty()))
            .map(|r| {
                general_discussion(
                    &r.node_id,
                    r.id,
                    r.body.unwrap_or_default(),
                    r.user,
                    r.submitted_at.unwrap_or_default(),
                )
            }),
    );
    let first = |d: &Discussion| d.notes.first().map(|n| n.created_at.clone());
    all.sort_by_key(first);
    all
}

const THREADS_QUERY: &str =
    "query($owner: String!, $name: String!, $number: Int!, $cursor: String) {
  repository(owner: $owner, name: $name) {
    pullRequest(number: $number) {
      reviewThreads(first: 100, after: $cursor) {
        pageInfo { hasNextPage endCursor }
        nodes {
          id isResolved isOutdated path line startLine diffSide startDiffSide subjectType
          comments(first: 100) { nodes { databaseId body createdAt author { login } } }
        }
      }
    }
  }
}";

const PENDING_QUERY: &str = "query($owner: String!, $name: String!, $number: Int!) {
  repository(owner: $owner, name: $name) {
    pullRequest(number: $number) {
      reviews(states: [PENDING], first: 1) {
        nodes { id databaseId body comments(first: 100) { nodes { databaseId body } } }
      }
    }
  }
}";

const ADD_THREAD: &str = "mutation($input: AddPullRequestReviewThreadInput!) {
  addPullRequestReviewThread(input: $input) { thread { comments(first: 1) { nodes { databaseId body } } } }
}";

const ADD_REPLY: &str = "mutation($input: AddPullRequestReviewThreadReplyInput!) {
  addPullRequestReviewThreadReply(input: $input) { comment { databaseId body } }
}";

const UPDATE_REVIEW: &str = "mutation($input: UpdatePullRequestReviewInput!) {
  updatePullRequestReview(input: $input) { pullRequestReview { databaseId body } }
}";

impl Forge for Github {
    fn kind(&self) -> ForgeKind {
        ForgeKind::Github
    }

    /// `github.com`, or `Enterprise Server <version>` from `GET /meta`.
    fn server_version(&self) -> Result<String> {
        if self.api == "https://api.github.com" {
            return Ok("github.com".into());
        }
        let meta: ApiMeta = self.get_json("/meta", &[])?;
        Ok(match meta.installed_version {
            Some(v) => format!("Enterprise Server {v}"),
            None => "Enterprise Server".into(),
        })
    }

    fn capabilities(&self) -> Result<Capabilities> {
        Ok(Capabilities::github(&self.server_version()?))
    }

    fn merge_request(&self, project: &str, iid: u64) -> Result<MergeRequest> {
        split(project)?;
        let pr: ApiPull = self.get_json(&format!("/repos/{project}/pulls/{iid}"), &[])?;
        Ok(MergeRequest {
            project: project.to_string(),
            iid: pr.number,
            title: pr.title,
            description: pr.body.unwrap_or_default(),
            source_branch: pr.head.ref_name,
            target_branch: pr.base.ref_name,
            // GitHub diffs from the merge base of the base branch and the
            // head: computed locally once both are fetched.
            base_sha: pr.base.sha.clone(),
            start_sha: pr.base.sha,
            head_sha: pr.head.sha,
            web_url: pr.html_url,
            forge: ForgeKind::Github,
            host: self.host.clone(),
        })
    }

    fn merge_request_for_branch(&self, project: &str, branch: &str) -> Result<u64> {
        let (owner, _) = split(project)?;
        let path = format!("/repos/{project}/pulls");
        let head = format!("{owner}:{branch}");
        let prs: Vec<ApiPullRef> = self.get_json(&path, &[("head", &head), ("state", "open")])?;
        if let Some(pr) = prs.first() {
            return Ok(pr.number);
        }
        // From a fork: the head owner is not the repository's.
        let prs: Vec<ApiPullRef> =
            self.get_json(&path, &[("state", "open"), ("per_page", "100")])?;
        prs.iter()
            .find(|p| p.head.ref_name == branch)
            .map(|p| p.number)
            .ok_or_else(|| ForgeError::NoMergeRequestForBranch(branch.to_string()))
    }

    fn discussions(&self, project: &str, iid: u64) -> Result<Vec<Discussion>> {
        let threads = self.review_threads(project, iid)?;
        let comments = self.get_all(&format!("/repos/{project}/issues/{iid}/comments"))?;
        let reviews = self.get_all(&format!("/repos/{project}/pulls/{iid}/reviews"))?;
        Ok(merge_discussions(threads, comments, reviews))
    }

    /// The comments of the current user's pending review, and its body.
    fn draft_notes(&self, project: &str, iid: u64) -> Result<Vec<DraftNote>> {
        let Some(r) = self.find_pending(project, iid)? else {
            return Ok(Vec::new());
        };
        let body = (!r.body.trim().is_empty()).then(|| DraftNote {
            id: r.database_id,
            note: r.body.clone(),
            position: None,
        });
        Ok(body
            .into_iter()
            .chain(r.comments.nodes.into_iter().map(|c| DraftNote {
                id: c.database_id,
                note: c.body,
                position: None,
            }))
            .collect())
    }

    fn create_draft_note(&self, project: &str, iid: u64, c: &NewComment) -> Result<u64> {
        let commit = c.position.as_ref().map(|p| p.head_sha.as_str());
        let pending = self.ensure_pending(project, iid, commit)?;
        let (mutation, mut input) = draft_mutation(c, &pending.node_id);
        Ok(match mutation {
            "addPullRequestReviewThread" => {
                let r: CreatedThread = self.graphql(ADD_THREAD, json!({ "input": input }))?;
                r.add
                    .thread
                    .comments
                    .nodes
                    .first()
                    .map(|c| c.database_id)
                    .ok_or_else(|| ForgeError::Graphql("thread created without comment".into()))?
            }
            "addPullRequestReviewThreadReply" => {
                let r: CreatedReply = self.graphql(ADD_REPLY, json!({ "input": input }))?;
                r.add.comment.database_id
            }
            _ => {
                let body = review_body(&pending.body, c);
                input["body"] = body.clone().into();
                let r: UpdatedReview = self.graphql(UPDATE_REVIEW, json!({ "input": input }))?;
                self.set_pending_body(&body);
                r.update.pull_request_review.database_id
            }
        })
    }

    /// Submits the pending review with the `COMMENT` event.
    fn publish_drafts(&self, project: &str, iid: u64) -> Result<()> {
        let cached = self
            .pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
            .filter(|p| p.project == project && p.number == iid);
        let (id, body) = match cached {
            Some(p) => (p.id, p.body),
            None => match self.find_pending(project, iid)? {
                Some(r) => (r.database_id, r.body),
                None => return Err(ForgeError::NoPendingReview),
            },
        };
        let mut payload = json!({ "event": "COMMENT" });
        if !body.trim().is_empty() {
            payload["body"] = body.into();
        }
        self.post::<Value>(
            &format!("/repos/{project}/pulls/{iid}/reviews/{id}/events"),
            &payload,
        )
        .map(drop)
    }

    fn post_comment(&self, project: &str, iid: u64, c: &NewComment) -> Result<()> {
        let (path, body) = direct_request(project, iid, c);
        if path == "/graphql" {
            self.graphql::<Value>(ADD_REPLY, json!({ "input": body["input"] }))
                .map(drop)
        } else {
            self.post::<Value>(&path, &body).map(drop)
        }
    }
}

#[derive(Deserialize)]
struct ApiPullRef {
    number: u64,
    head: ApiBranch,
}

#[cfg(test)]
mod tests;
