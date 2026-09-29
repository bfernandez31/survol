//! GitHub client tests: payloads, token lookup, and responses recorded from
//! BurntSushi/ripgrep#3529 (trimmed; the `PRRT_file` and `PRRT_left`
//! threads are added by hand), served by the fake HTTP server.

use serde_json::json;

use super::*;
use crate::forge::fake::{self, Response, ok, status};
use crate::forge::{LinePoint, LineRange};

const PULL: &str = include_str!("../fixtures/github/pull.json");
const THREADS: &str = include_str!("../fixtures/github/review_threads.json");
const REVIEWS: &str = include_str!("../fixtures/github/reviews.json");
const ISSUE_COMMENTS: &str = include_str!("../fixtures/github/issue_comments.json");

fn client(addr: &str) -> Github {
    Github::new(
        "github.test",
        addr,
        &format!("{addr}/graphql"),
        "tok".into(),
        None,
    )
    .unwrap()
}

fn pos(new_line: Option<u32>, old_line: Option<u32>) -> Position {
    Position {
        position_type: "text".into(),
        base_sha: "b0".into(),
        start_sha: "s0".into(),
        head_sha: "h0".into(),
        old_path: "src/Old.java".into(),
        new_path: "src/New.java".into(),
        old_line,
        new_line,
        line_range: None,
    }
}

fn point(old_line: Option<u32>, new_line: Option<u32>) -> LinePoint {
    LinePoint {
        line_code: String::new(),
        kind: Some("old".into()),
        old_line,
        new_line,
    }
}

fn ranged(mut p: Position, start: LinePoint) -> Position {
    let end = point(p.old_line, p.new_line);
    p.line_range = Some(LineRange { start, end });
    p
}

#[test]
fn endpoints_and_links() {
    assert_eq!(
        api_urls("github.com"),
        (
            "https://api.github.com".into(),
            "https://api.github.com/graphql".into()
        )
    );
    assert_eq!(
        api_urls("https://github.corp.example/"),
        (
            "https://github.corp.example/api/v3".into(),
            "https://github.corp.example/api/graphql".into()
        )
    );
    let link = r#"<https://api.github.com/repositories/1/pulls/3/reviews?page=2>; rel="next", <https://api.github.com/repositories/1/pulls/3/reviews?page=3>; rel="last""#;
    assert_eq!(
        next_link(link).as_deref(),
        Some("https://api.github.com/repositories/1/pulls/3/reviews?page=2")
    );
    assert_eq!(next_link(r#"<https://x/?page=1>; rel="prev""#), None);
}

#[test]
fn token_lookup_order() {
    let env = |vars: &'static [(&'static str, &'static str)]| {
        move |k: &str| {
            vars.iter()
                .find(|(n, _)| *n == k)
                .map(|(_, v)| v.to_string())
        }
    };
    fn gh(_: &str) -> Option<String> {
        Some("from-gh".into())
    }
    fn no_gh(_: &str) -> Option<String> {
        None
    }
    assert_eq!(
        token_from(
            "github.com",
            Some("cfg"),
            env(&[("GH_TOKEN", "b"), ("GITHUB_TOKEN", "a")]),
            gh
        ),
        Some(("a".into(), "GITHUB_TOKEN"))
    );
    assert_eq!(
        token_from("github.com", Some("cfg"), env(&[("GH_TOKEN", " b ")]), gh),
        Some(("b".into(), "GH_TOKEN"))
    );
    assert_eq!(
        token_from(
            "github.com",
            Some("cfg"),
            env(&[("GITHUB_TOKEN", "  ")]),
            gh
        ),
        Some(("cfg".into(), "[github] token"))
    );
    assert_eq!(
        token_from("github.com", None, env(&[]), gh),
        Some(("from-gh".into(), "gh"))
    );
    assert_eq!(token_from("github.com", None, env(&[]), no_gh), None);
    // Enterprise Server: gh's enterprise variables, not github.com's token.
    assert_eq!(
        token_from(
            "github.corp.example",
            None,
            env(&[("GITHUB_TOKEN", "dotcom"), ("GH_ENTERPRISE_TOKEN", "e")]),
            no_gh
        ),
        Some(("e".into(), "GH_ENTERPRISE_TOKEN"))
    );
    assert_eq!(
        token_from(
            "github.corp.example",
            None,
            env(&[("GITHUB_TOKEN", "dotcom")]),
            gh
        ),
        Some(("from-gh".into(), "gh"))
    );
}

#[test]
fn payloads_for_lines_ranges_and_files() {
    // An added line: right side, new number, on the new path.
    assert_eq!(
        rest_comment_payload("Why?", &pos(Some(11), None)),
        json!({"body": "Why?", "commit_id": "h0", "path": "src/New.java",
               "line": 11, "side": "RIGHT"})
    );
    // An unchanged line: right side too.
    assert_eq!(
        rest_comment_payload("x", &pos(Some(5), Some(4)))["side"],
        "RIGHT"
    );
    // A removed line: left side, old number.
    assert_eq!(
        rest_comment_payload("Gone?", &pos(None, Some(7))),
        json!({"body": "Gone?", "commit_id": "h0", "path": "src/New.java",
               "line": 7, "side": "LEFT"})
    );
    // A range from a removed line to an added one.
    let range = ranged(pos(Some(12), None), point(Some(9), None));
    assert_eq!(
        rest_comment_payload("Range", &range),
        json!({"body": "Range", "commit_id": "h0", "path": "src/New.java",
               "line": 12, "side": "RIGHT", "start_line": 9, "start_side": "LEFT"})
    );
    // A range from an unchanged line to a removed one: both on the left.
    let range = ranged(pos(None, Some(2)), point(Some(1), Some(1)));
    assert_eq!(
        thread_input("R", &range, "PRR_1"),
        json!({"pullRequestReviewId": "PRR_1", "body": "R", "path": "src/New.java",
               "subjectType": "LINE", "line": 2, "side": "LEFT",
               "startLine": 1, "startSide": "LEFT"})
    );
    // From an added line to a removed one: not expressible, single line.
    let range = ranged(pos(None, Some(2)), point(None, Some(3)));
    assert_eq!(thread_input("R", &range, "PRR_1").get("startLine"), None);
    // A one-line "range" is a line.
    let range = ranged(pos(Some(4), None), point(None, Some(4)));
    assert_eq!(rest_comment_payload("x", &range).get("start_line"), None);
    // A whole file.
    let mut file = pos(None, None);
    file.position_type = "file".into();
    assert_eq!(
        rest_comment_payload("File", &file),
        json!({"body": "File", "commit_id": "h0", "path": "src/New.java",
               "subject_type": "file"})
    );
    assert_eq!(
        thread_input("File", &file, "PRR_1"),
        json!({"pullRequestReviewId": "PRR_1", "body": "File", "path": "src/New.java",
               "subjectType": "FILE"})
    );
    assert_eq!(
        create_review_payload(Some("h0")),
        json!({"commit_id": "h0"})
    );
    assert_eq!(create_review_payload(None), json!({}));
}

#[test]
fn mutations_and_direct_requests() {
    let reply = NewComment {
        body: "Agreed.".into(),
        position: None,
        in_reply_to: Some("PRRT_abc".into()),
    };
    assert_eq!(
        draft_mutation(&reply, "PRR_1"),
        (
            "addPullRequestReviewThreadReply",
            json!({"pullRequestReviewId": "PRR_1", "pullRequestReviewThreadId": "PRRT_abc",
                   "body": "Agreed."})
        )
    );
    assert_eq!(
        direct_request("o/r", 7, &reply),
        (
            "/graphql".into(),
            json!({"mutation": "addPullRequestReviewThreadReply",
                   "input": {"pullRequestReviewThreadId": "PRRT_abc", "body": "Agreed."}})
        )
    );
    let general = NewComment {
        body: "@alice Thanks".into(),
        position: None,
        in_reply_to: Some(format!("{GENERAL}IC_1")),
    };
    assert_eq!(
        draft_mutation(&general, "PRR_1").0,
        "updatePullRequestReview"
    );
    assert_eq!(
        direct_request("o/r", 7, &general).0,
        "/repos/o/r/issues/7/comments"
    );
    let summary = NewComment {
        body: "Overall fine.".into(),
        position: None,
        in_reply_to: None,
    };
    assert_eq!(
        draft_mutation(&summary, "PRR_1"),
        (
            "updatePullRequestReview",
            json!({"pullRequestReviewId": "PRR_1", "body": "Overall fine."})
        )
    );
    let line = NewComment {
        body: "Hm".into(),
        position: Some(pos(Some(3), None)),
        in_reply_to: None,
    };
    assert_eq!(
        draft_mutation(&line, "PRR_1").0,
        "addPullRequestReviewThread"
    );
    assert_eq!(
        direct_request("o/r", 7, &line).0,
        "/repos/o/r/pulls/7/comments"
    );

    // The overall comment goes on top of the review body, replies after.
    assert_eq!(review_body("", &summary), "Overall fine.");
    assert_eq!(
        review_body("@alice Thanks", &summary),
        "Overall fine.\n\n@alice Thanks"
    );
    assert_eq!(
        review_body("Overall fine.", &general),
        "Overall fine.\n\n@alice Thanks"
    );
}

#[test]
fn fetches_the_pull_request() {
    let (addr, rx) = fake::serve(vec![ok(PULL)]);
    let gh = client(&addr);
    let mr = gh.merge_request("BurntSushi/ripgrep", 3529).unwrap();
    assert_eq!(mr.iid, 3529);
    assert_eq!(mr.title, "Implement glob-not `-G`");
    assert!(mr.description.starts_with("New options"));
    assert_eq!(mr.source_branch, "new-feature/glob-not");
    assert_eq!(mr.target_branch, "master");
    assert_eq!(mr.head_sha, "2c17dbd768fa141b14aa9ce42e9ba681e2945813");
    // The merge base is computed locally: base and start are the base tip.
    assert_eq!(mr.base_sha, "3fce3b5bb0236da2df6d99672afb8a719642eca7");
    assert_eq!(mr.start_sha, mr.base_sha);
    assert_eq!(mr.forge, ForgeKind::Github);
    assert_eq!(mr.host, "github.test");
    assert_eq!(mr.reference(), "#3529");
    let r = rx.recv().unwrap();
    assert_eq!(
        (r.method.as_str(), r.path.as_str()),
        ("GET", "/repos/BurntSushi/ripgrep/pulls/3529")
    );
    assert_eq!(r.authorization, "Bearer tok");
    assert!(gh.merge_request("not-a-repo", 1).is_err());
}

#[test]
fn maps_threads_comments_and_reviews() {
    let (addr, rx) = fake::serve(vec![ok(THREADS), ok(ISSUE_COMMENTS), ok(REVIEWS)]);
    let all = client(&addr)
        .discussions("BurntSushi/ripgrep", 3529)
        .unwrap();
    let ids: Vec<&str> = all.iter().map(|d| d.id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "PRRT_kwDOAzJbyc6gTMKt",
            "PRRT_kwDOAzJbyc6gTMbX",
            "PRRT_kwDOAzJbyc6gTOdk",
            "PRRT_kwDOAzJbyc6gTPgI",
            "general:PRR_kwDOAzJbyc8AAAABMpUCZA",
            "PRRT_file",
            "PRRT_left",
            "general:IC_kwDOAzJbyc8AAAABWSzLkQ",
        ],
        "oldest first; reviews without a body are left out"
    );

    // Outdated thread: resolved, on its file, no line.
    let d = &all[0];
    assert!(d.is_resolvable() && d.is_resolved() && !d.individual_note);
    assert_eq!(d.notes.len(), 2);
    assert_eq!(d.notes[1].author.username, "yilmazhasan");
    let p = d.position().unwrap();
    assert_eq!(
        (p.new_path.as_str(), p.new_line, p.old_line),
        ("tests/misc.rs", None, None)
    );
    // A line on the right side.
    let p = all[2].position().unwrap();
    assert_eq!(
        (p.position_type.as_str(), p.new_line, p.old_line),
        ("text", Some(2763), None)
    );
    assert_eq!(all[2].notes[0].id, 3959462471);
    // A general review body.
    let review = &all[4];
    assert!(review.individual_note && !review.is_resolvable());
    assert!(review.position().is_none());
    assert_eq!(review.notes[0].author.username, "ttencate");
    // A file thread, by a deleted account.
    let file = &all[5];
    assert!(!file.is_resolved());
    assert_eq!(file.position().unwrap().position_type, "file");
    assert_eq!(file.notes[0].author.username, "ghost");
    // The left side: an old line.
    let p = all[6].position().unwrap();
    assert_eq!((p.new_line, p.old_line), (None, Some(12)));
    assert_eq!(all[7].notes[0].author.username, "MichaReiser");

    let reqs: Vec<fake::Request> = rx.try_iter().collect();
    assert_eq!(
        (reqs[0].method.as_str(), reqs[0].path.as_str()),
        ("POST", "/graphql")
    );
    let q = reqs[0].json();
    assert_eq!(
        q["variables"],
        json!({"owner": "BurntSushi", "name": "ripgrep", "number": 3529, "cursor": null})
    );
    assert!(q["query"].as_str().unwrap().contains("reviewThreads"));
    assert_eq!(
        reqs[1].path,
        "/repos/BurntSushi/ripgrep/issues/3529/comments?per_page=100"
    );
    assert_eq!(
        reqs[2].path,
        "/repos/BurntSushi/ripgrep/pulls/3529/reviews?per_page=100"
    );
}

#[test]
fn follows_pages() {
    let empty_threads = r#"{"data":{"repository":{"pullRequest":{"reviewThreads":{
        "pageInfo":{"hasNextPage":false,"endCursor":null},"nodes":[]}}}}}"#;
    let threads_page1 = r#"{"data":{"repository":{"pullRequest":{"reviewThreads":{
        "pageInfo":{"hasNextPage":true,"endCursor":"C1"},"nodes":[]}}}}}"#;
    let page1 = r#"[{"id":1,"node_id":"IC_1","body":"one","user":{"login":"a"},
        "created_at":"2026-01-01T00:00:00Z"}]"#;
    let page2 = r#"[{"id":2,"node_id":"IC_2","body":"two","user":{"login":"b"},
        "created_at":"2026-01-02T00:00:00Z"}]"#;
    let (addr, rx) = fake::serve(vec![
        ok(threads_page1),
        ok(empty_threads),
        Response {
            status: "200 OK",
            headers: vec![(
                "Link",
                r#"<{addr}/repositories/9/issues/5/comments?per_page=100&page=2>; rel="next""#
                    .into(),
            )],
            body: page1.into(),
        },
        ok(page2),
        ok("[]"),
    ]);
    let all = client(&addr).discussions("o/r", 5).unwrap();
    assert_eq!(all.len(), 2);
    assert_eq!(all[1].notes[0].body, "two");
    let reqs: Vec<fake::Request> = rx.try_iter().collect();
    assert_eq!(reqs[1].json()["variables"]["cursor"], "C1");
    assert_eq!(
        reqs[3].path,
        "/repositories/9/issues/5/comments?per_page=100&page=2"
    );
}

#[test]
fn finds_the_pull_request_of_a_branch() {
    let (addr, rx) = fake::serve(vec![
        ok("[]"),
        ok(r#"[{"number":4,"head":{"ref":"other","sha":"x"}},
               {"number":9,"head":{"ref":"feat/x","sha":"y"}}]"#),
        ok("[]"),
        ok("[]"),
    ]);
    let gh = client(&addr);
    assert_eq!(gh.merge_request_for_branch("o/r", "feat/x").unwrap(), 9);
    let reqs: Vec<fake::Request> = rx.try_iter().collect();
    assert_eq!(
        reqs[0].path,
        "/repos/o/r/pulls?head=o%3Afeat%2Fx&state=open"
    );
    assert!(matches!(
        gh.merge_request_for_branch("o/r", "none"),
        Err(ForgeError::NoMergeRequestForBranch(_))
    ));
}

#[test]
fn lists_the_pending_review_as_draft_notes() {
    let pending = r#"{"data":{"repository":{"pullRequest":{"reviews":{"nodes":[
        {"id":"PRR_1","databaseId":55,"body":"Summary so far",
         "comments":{"nodes":[{"databaseId":201,"body":"a"},{"databaseId":202,"body":"b"}]}}]}}}}}"#;
    let none = r#"{"data":{"repository":{"pullRequest":{"reviews":{"nodes":[]}}}}}"#;
    let (addr, _rx) = fake::serve(vec![ok(pending), ok(none)]);
    let gh = client(&addr);
    let ids: Vec<u64> = gh
        .draft_notes("o/r", 5)
        .unwrap()
        .iter()
        .map(|d| d.id)
        .collect();
    assert_eq!(ids, [55, 201, 202]);
    assert!(gh.draft_notes("o/r", 5).unwrap().is_empty());
}

#[test]
fn reports_graphql_and_http_errors() {
    let (addr, _rx) = fake::serve(vec![
        ok(r#"{"data":null,"errors":[{"message":"Could not resolve to a PullRequest"}]}"#),
        status("401 Unauthorized", r#"{"message":"Bad credentials"}"#),
    ]);
    let gh = client(&addr);
    match gh.discussions("o/r", 5) {
        Err(ForgeError::Graphql(m)) => assert!(m.contains("Could not resolve")),
        other => panic!("unexpected {other:?}"),
    }
    match gh.merge_request("o/r", 5) {
        Err(ForgeError::Status { status: 401, .. }) => {}
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn versions_and_capabilities() {
    let (addr, _rx) = fake::serve(vec![ok(r#"{"installed_version":"3.14.2"}"#)]);
    let caps = client(&addr).capabilities().unwrap();
    assert_eq!(caps.version, "Enterprise Server 3.14.2");
    assert!(caps.draft_notes && caps.file_comments);
    assert_eq!(caps.forge, ForgeKind::Github);
}
