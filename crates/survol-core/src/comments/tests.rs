use serde_json::json;

use super::*;
use crate::forge::ForgeKind;
use crate::forge::fake::{self, ok, status};
use crate::forge::gitlab::Gitlab;

/// Hunk 0 (src/App.java): ` a`, `-b`, `+B`, `+B2`, ` c`, `-d`.
/// Hunk 1: renamed src/Old.java → src/New.java, ` x`, `-y`, `+z`.
/// Hunk 2: deleted src/Gone.java, `-p`, `-q`. Hunk 3: added src/Added.java, `+m`, `+n`.
const RAW: &str = "diff --git a/src/App.java b/src/App.java
index 1111111..2222222 100644
--- a/src/App.java
+++ b/src/App.java
@@ -10,4 +10,4 @@ class App
 a
-b
+B
+B2
 c
-d
diff --git a/src/Old.java b/src/New.java
similarity index 90%
rename from src/Old.java
rename to src/New.java
index 3333333..4444444 100644
--- a/src/Old.java
+++ b/src/New.java
@@ -1,2 +1,2 @@
 x
-y
+z
diff --git a/src/Gone.java b/src/Gone.java
deleted file mode 100644
index 5555555..0000000
--- a/src/Gone.java
+++ /dev/null
@@ -1,2 +0,0 @@
-p
-q
diff --git a/src/Added.java b/src/Added.java
new file mode 100644
index 0000000..6666666
--- /dev/null
+++ b/src/Added.java
@@ -0,0 +1,2 @@
+m
+n
";

const APP: &str = "495c302a84af98a5b0c227ee151874875266d305";
const NEW: &str = "6ed76b69da9b74c3b1cef12fb928c284bcd212e4";
const GONE: &str = "f6856fc96c0f2b790121733b0cb67cc6dba84bcc";
const ADDED: &str = "547aff9b74ca885ac038554222da29d9c77ac1bc";

fn diff() -> Diff {
    crate::diff::parse(RAW.as_bytes()).unwrap()
}

fn shas() -> Shas {
    Shas {
        base: "b0".into(),
        start: "s0".into(),
        head: "h0".into(),
    }
}

fn payload(pos: Position) -> serde_json::Value {
    draft_note_payload(&NewComment {
        body: "c".into(),
        position: Some(pos),
        in_reply_to: None,
    })
}

fn text_position(old_path: &str, new_path: &str) -> serde_json::Value {
    json!({
        "position_type": "text",
        "base_sha": "b0",
        "start_sha": "s0",
        "head_sha": "h0",
        "old_path": old_path,
        "new_path": new_path,
    })
}

#[test]
fn sha1_matches_known_vectors() {
    assert_eq!(sha1_hex(b""), "da39a3ee5e6b4b0d3255bfef95601890afd80709");
    assert_eq!(sha1_hex(b"abc"), "a9993e364706816aba3e25717850c26c9cd0d89d");
    assert_eq!(sha1_hex(b"src/App.java"), APP);
    // Over one 64-byte block.
    assert_eq!(
        sha1_hex("a".repeat(200).as_bytes()),
        "e61cfffe0d9195a525fc6cf06ca2d77119c24a40"
    );
}

#[test]
fn positions_of_added_removed_and_context_lines() {
    let d = diff();
    let s = shas();

    // Added line: new_line only.
    let mut want = text_position("src/App.java", "src/App.java");
    want["new_line"] = json!(11);
    assert_eq!(
        payload(line_position(&d, 0, 2, None, &s)),
        json!({"note": "c", "position": want})
    );

    // Removed line: old_line only.
    let mut want = text_position("src/App.java", "src/App.java");
    want["old_line"] = json!(11);
    assert_eq!(
        payload(line_position(&d, 0, 1, None, &s)),
        json!({"note": "c", "position": want})
    );

    // Unchanged line: both.
    let mut want = text_position("src/App.java", "src/App.java");
    want["old_line"] = json!(12);
    want["new_line"] = json!(13);
    assert_eq!(
        payload(line_position(&d, 0, 4, None, &s)),
        json!({"note": "c", "position": want})
    );
}

#[test]
fn positions_of_renamed_deleted_and_added_files() {
    let d = diff();
    let s = shas();

    // Renamed: old_path ≠ new_path.
    let mut want = text_position("src/Old.java", "src/New.java");
    want["new_line"] = json!(2);
    assert_eq!(
        serde_json::to_value(line_position(&d, 1, 2, None, &s)).unwrap(),
        want
    );
    let mut want = text_position("src/Old.java", "src/New.java");
    want["old_line"] = json!(2);
    assert_eq!(
        serde_json::to_value(line_position(&d, 1, 1, None, &s)).unwrap(),
        want
    );

    // Deleted: the same path on both sides, old lines.
    let mut want = text_position("src/Gone.java", "src/Gone.java");
    want["old_line"] = json!(2);
    assert_eq!(
        serde_json::to_value(line_position(&d, 2, 1, None, &s)).unwrap(),
        want
    );

    // Added file: new lines.
    let mut want = text_position("src/Added.java", "src/Added.java");
    want["new_line"] = json!(1);
    assert_eq!(
        serde_json::to_value(line_position(&d, 3, 0, None, &s)).unwrap(),
        want
    );

    // Whole file.
    assert_eq!(
        serde_json::to_value(file_position(&d, 1, &s)).unwrap(),
        json!({
            "position_type": "file",
            "base_sha": "b0",
            "start_sha": "s0",
            "head_sha": "h0",
            "old_path": "src/Old.java",
            "new_path": "src/New.java",
        })
    );
}

#[test]
fn range_positions_carry_line_codes() {
    let d = diff();
    let s = shas();
    // From the removed `b` to the unchanged `c`: GitLab's counters are
    // (old 11, new 11) and (old 12, new 13).
    let mut want = text_position("src/App.java", "src/App.java");
    want["old_line"] = json!(12);
    want["new_line"] = json!(13);
    want["line_range"] = json!({
        "start": {"line_code": format!("{APP}_11_11"), "type": "old", "old_line": 11},
        "end": {"line_code": format!("{APP}_12_13"), "type": "old", "old_line": 12, "new_line": 13},
    });
    assert_eq!(
        serde_json::to_value(line_position(&d, 0, 4, Some(1), &s)).unwrap(),
        want
    );
    // Added lines keep the old counter.
    let p = line_position(&d, 0, 3, Some(2), &s);
    let r = p.line_range.unwrap();
    assert_eq!(r.start.line_code, format!("{APP}_12_11"));
    assert_eq!(r.start.kind.as_deref(), Some("new"));
    assert_eq!(r.end.line_code, format!("{APP}_12_12"));
    assert_eq!((p.old_line, p.new_line), (None, Some(12)));
    // New and deleted files: counters start at 0 on the missing side.
    let p = line_position(&d, 3, 1, Some(0), &s);
    assert_eq!(p.line_range.unwrap().end.line_code, format!("{ADDED}_0_2"));
    let p = line_position(&d, 2, 1, Some(0), &s);
    let r = p.line_range.unwrap();
    assert_eq!(r.start.line_code, format!("{GONE}_1_0"));
    assert_eq!(r.end.line_code, format!("{GONE}_2_0"));
    let p = line_position(&d, 1, 2, Some(0), &s);
    assert_eq!(p.line_range.unwrap().start.line_code, format!("{NEW}_1_1"));
    // A range of one line is a line.
    assert!(line_position(&d, 0, 2, Some(2), &s).line_range.is_none());
}

/// App.java moved down by 5 lines, same content; New.java's hunk changed.
const RAW_V2: &str = "diff --git a/src/App.java b/src/App.java
--- a/src/App.java
+++ b/src/App.java
@@ -15,4 +15,4 @@ class App
 a
-b
+B
+B2
 c
-d
diff --git a/src/Old.java b/src/New.java
similarity index 80%
rename from src/Old.java
rename to src/New.java
--- a/src/Old.java
+++ b/src/New.java
@@ -1,2 +1,3 @@
 x
-y
+w
+z
";

#[test]
fn drafts_follow_their_line_across_versions() {
    let d1 = diff();
    let mut store = CommentStore::default();
    let on_b2 = store.add(
        Anchor::Line {
            start: Some(line_anchor(&d1, 0, 1)),
            line: line_anchor(&d1, 0, 3),
        },
        "range",
    );
    let on_z = store.add(
        Anchor::Line {
            start: None,
            line: line_anchor(&d1, 1, 2),
        },
        "on z",
    );
    let on_q = store.add(
        Anchor::Line {
            start: None,
            line: line_anchor(&d1, 2, 1),
        },
        "on q",
    );
    let on_file = store.add(file_anchor(&d1, 3), "file");
    let dir = tempfile::tempdir().unwrap();
    let path = comments_path(dir.path(), "mr-1");
    store.save(&path).unwrap();
    let store = CommentStore::load(&path).unwrap();
    assert_eq!(store.drafts.len(), 4);

    let placed = |d: &Diff, id| place(&store.get(id).unwrap().anchor, d);
    assert_eq!(
        placed(&d1, on_b2),
        Placement::Line {
            file: 0,
            hunk: 0,
            line: 3,
            start: Some(1),
            moved: false
        }
    );

    let d2 = crate::diff::parse(RAW_V2.as_bytes()).unwrap();
    // Same hunk content, other line numbers: same place.
    assert_eq!(
        placed(&d2, on_b2),
        Placement::Line {
            file: 0,
            hunk: 0,
            line: 3,
            start: Some(1),
            moved: false
        }
    );
    assert_eq!(
        describe(&d2, placed(&d2, on_b2)),
        "src/App.java:16 (old)-17"
    );
    // The hunk changed but the line is still there: moved.
    assert_eq!(
        placed(&d2, on_z),
        Placement::Line {
            file: 1,
            hunk: 1,
            line: 3,
            start: None,
            moved: true
        }
    );
    // File and lines gone: stale.
    assert!(placed(&d2, on_q).is_stale());
    assert!(placed(&d2, on_file).is_stale());

    // A stale draft is not published.
    let caps = Capabilities::from_version("17.0.0");
    let p = plan(&store, &d2, &shas(), &caps);
    assert_eq!(p.comments.len(), 2);
    assert_eq!(
        p.skipped.iter().map(|s| s.0).collect::<Vec<_>>(),
        [on_q, on_file]
    );
}

#[test]
fn remote_positions_find_their_line() {
    let d = diff();
    let pos = |old: Option<u32>, new: Option<u32>, path: &str| {
        Position {
            old_line: old,
            new_line: new,
            ..line_position(&d, 0, 0, None, &shas())
        }
        .with_path(path)
    };
    assert_eq!(
        place_position(&pos(None, Some(12), "src/App.java"), &d),
        Some((0, Some((0, 3))))
    );
    assert_eq!(
        place_position(&pos(Some(13), None, "src/App.java"), &d),
        Some((0, Some((0, 5))))
    );
    assert_eq!(
        place_position(&pos(Some(12), Some(13), "src/App.java"), &d),
        Some((0, Some((0, 4))))
    );
    // Outside the hunks: the file only.
    assert_eq!(
        place_position(&pos(None, Some(99), "src/App.java"), &d),
        Some((0, None))
    );
    assert_eq!(place_position(&pos(None, Some(1), "nope"), &d), None);
}

impl Position {
    fn with_path(mut self, p: &str) -> Self {
        self.old_path = p.into();
        self.new_path = p.into();
        self
    }
}

fn review_store(d: &Diff) -> CommentStore {
    let mut store = CommentStore::default();
    store.add(
        Anchor::Line {
            start: None,
            line: line_anchor(d, 0, 2),
        },
        "Why a second B?",
    );
    store.add(
        Anchor::Line {
            start: Some(line_anchor(d, 1, 0)),
            line: line_anchor(d, 1, 1),
        },
        "Renamed on purpose?",
    );
    store.add(file_anchor(d, 2), "Deleting this is fine.");
    store.summary = "Looks consistent overall.".into();
    store
}

#[test]
fn replies_answer_existing_discussions() {
    let d = diff();
    let mut store = CommentStore::default();
    store.add(
        Anchor::Reply {
            discussion: "abc123".into(),
            author: "alice".into(),
        },
        "Agreed, fixed.",
    );
    let drafts = plan(&store, &d, &shas(), &Capabilities::from_version("17.0.0"));
    assert_eq!(drafts.comments[0].what, "reply to @alice");
    assert_eq!(
        drafts.requests("p", "7")[0].body,
        Some(json!({"note": "Agreed, fixed.", "in_reply_to_discussion_id": "abc123"}))
    );
    let direct = plan(&store, &d, &shas(), &Capabilities::from_version("15.0.0"));
    let r = &direct.requests("p", "7")[0];
    assert_eq!(
        r.path,
        "/projects/p/merge_requests/7/discussions/abc123/notes"
    );
    assert_eq!(r.body, Some(json!({"body": "Agreed, fixed."})));
}

#[test]
fn plans_drafts_or_direct_discussions() {
    let d = diff();
    let store = review_store(&d);
    let p = plan(&store, &d, &shas(), &Capabilities::from_version("17.2.0"));
    assert_eq!(p.mode, Mode::Drafts);
    let what: Vec<&str> = p.comments.iter().map(|c| c.what.as_str()).collect();
    assert_eq!(
        what,
        [
            "src/App.java:11",
            "src/New.java:1-2 (old)",
            "src/Gone.java",
            "summary"
        ]
    );
    let reqs = p.requests("grp%2Fapp", "7");
    let paths: Vec<&str> = reqs.iter().map(|r| r.path.as_str()).collect();
    assert_eq!(
        paths,
        [
            "/projects/grp%2Fapp/merge_requests/7/draft_notes",
            "/projects/grp%2Fapp/merge_requests/7/draft_notes",
            "/projects/grp%2Fapp/merge_requests/7/draft_notes",
            "/projects/grp%2Fapp/merge_requests/7/draft_notes",
            "/projects/grp%2Fapp/merge_requests/7/draft_notes/bulk_publish",
        ]
    );
    assert_eq!(
        reqs[2].body.as_ref().unwrap()["position"]["position_type"],
        "file"
    );
    assert_eq!(
        reqs[3].body,
        Some(json!({"note": "Looks consistent overall."}))
    );
    assert_eq!(reqs[4].body, None);

    // Old instance: discussions right away; a file comment becomes a
    // general one naming the file.
    let p = plan(&store, &d, &shas(), &Capabilities::from_version("15.4.0"));
    assert_eq!(p.mode, Mode::Direct);
    let reqs = p.requests("grp%2Fapp", "7");
    assert_eq!(reqs.len(), 4);
    assert!(reqs.iter().all(|r| r.path.ends_with("/discussions")));
    assert_eq!(
        reqs[2].body,
        Some(json!({"body": "**`src/Gone.java`**\n\nDeleting this is fine."}))
    );
    assert_eq!(reqs[0].body.as_ref().unwrap()["body"], "Why a second B?");
}

#[test]
fn publishes_through_gitlab_draft_notes() {
    let d = diff();
    let dir = tempfile::tempdir().unwrap();
    let path = comments_path(dir.path(), "mr-7");
    let mut store = review_store(&d);
    store.save(&path).unwrap();
    let p = plan(&store, &d, &shas(), &Capabilities::from_version("17.2.0"));

    let (addr, rx) = fake::serve(vec![
        ok(r#"{"id":101}"#),
        ok(r#"{"id":102}"#),
        ok(r#"{"id":103}"#),
        ok(r#"{"id":104}"#),
        status("204 No Content", ""),
    ]);
    let gl = Gitlab::new(&addr, "tok".into(), None).unwrap();
    let n = publish(&gl, "grp/app", 7, &p, &mut store, &path, &mut |_| {}).unwrap();
    assert_eq!(n, 4);

    let reqs: Vec<fake::Request> = rx.try_iter().collect();
    assert_eq!(reqs.len(), 5);
    for r in &reqs[..4] {
        assert_eq!(r.method, "POST");
        assert_eq!(
            r.path,
            "/api/v4/projects/grp%2Fapp/merge_requests/7/draft_notes"
        );
        assert_eq!(r.authorization, "Bearer tok");
        assert_eq!(r.content_type, "application/json");
    }
    // The exact JSON sent for the added line and the range.
    assert_eq!(
        reqs[0].json(),
        json!({
            "note": "Why a second B?",
            "position": {
                "position_type": "text",
                "base_sha": "b0", "start_sha": "s0", "head_sha": "h0",
                "old_path": "src/App.java", "new_path": "src/App.java",
                "new_line": 11
            }
        })
    );
    assert_eq!(
        reqs[1].json(),
        json!({
            "note": "Renamed on purpose?",
            "position": {
                "position_type": "text",
                "base_sha": "b0", "start_sha": "s0", "head_sha": "h0",
                "old_path": "src/Old.java", "new_path": "src/New.java",
                "old_line": 2,
                "line_range": {
                    "start": {"line_code": format!("{NEW}_1_1"), "type": "old", "old_line": 1, "new_line": 1},
                    "end": {"line_code": format!("{NEW}_2_2"), "type": "old", "old_line": 2}
                }
            }
        })
    );
    assert_eq!(reqs[3].json(), json!({"note": "Looks consistent overall."}));
    assert_eq!(
        (reqs[4].method.as_str(), reqs[4].path.as_str()),
        (
            "POST",
            "/api/v4/projects/grp%2Fapp/merge_requests/7/draft_notes/bulk_publish"
        )
    );
    assert!(reqs[4].body.is_empty());

    let saved = CommentStore::load(&path).unwrap();
    assert!(saved.drafts.is_empty());
    assert_eq!(saved.published.len(), 3);
    assert!(saved.summary.is_empty());
    assert!(saved.published.iter().all(|d| d.published_at.is_some()));
}

#[test]
fn an_interrupted_publication_resumes_without_duplicates() {
    let d = diff();
    let dir = tempfile::tempdir().unwrap();
    let path = comments_path(dir.path(), "mr-7");
    let mut store = review_store(&d);
    let caps = Capabilities::from_version("17.2.0");
    let p = plan(&store, &d, &shas(), &caps);

    let (addr, rx) = fake::serve(vec![
        ok(r#"{"id":101}"#),
        status("500 Internal Server Error", r#"{"message":"boom"}"#),
    ]);
    let gl = Gitlab::new(&addr, "tok".into(), None).unwrap();
    let err = publish(&gl, "grp/app", 7, &p, &mut store, &path, &mut |_| {}).unwrap_err();
    assert!(err.to_string().contains("after 1 of 4"), "{err}");
    assert_eq!(rx.try_iter().count(), 2);
    let saved = CommentStore::load(&path).unwrap();
    assert_eq!(saved.drafts[0].remote_id, Some(101));
    assert_eq!(saved.drafts.len(), 3);

    // Again: the first draft note exists already.
    let mut store = saved;
    let p = plan(&store, &d, &shas(), &caps);
    assert_eq!(p.requests("p", "7").len(), 4);
    let (addr, rx) = fake::serve(vec![
        ok(r#"{"id":102}"#),
        ok(r#"{"id":103}"#),
        ok(r#"{"id":104}"#),
        status("204 No Content", ""),
    ]);
    let gl = Gitlab::new(&addr, "tok".into(), None).unwrap();
    publish(&gl, "grp/app", 7, &p, &mut store, &path, &mut |_| {}).unwrap();
    let reqs: Vec<fake::Request> = rx.try_iter().collect();
    assert_eq!(reqs.len(), 4);
    assert_eq!(reqs[0].json()["note"], "Renamed on purpose?");
    assert!(store.drafts.is_empty());
}

#[test]
fn publishes_directly_on_old_instances() {
    let d = diff();
    let dir = tempfile::tempdir().unwrap();
    let path = comments_path(dir.path(), "mr-7");
    let mut store = review_store(&d);
    let p = plan(&store, &d, &shas(), &Capabilities::from_version("15.0.0"));
    let (addr, rx) = fake::serve(vec![
        ok(r#"{"id":"d1"}"#),
        ok(r#"{"id":"d2"}"#),
        status("403 Forbidden", r#"{"message":"403 Forbidden"}"#),
    ]);
    let gl = Gitlab::new(&addr, "tok".into(), None).unwrap();
    assert!(publish(&gl, "grp/app", 7, &p, &mut store, &path, &mut |_| {}).is_err());
    let reqs: Vec<fake::Request> = rx.try_iter().collect();
    assert_eq!(
        reqs[0].path,
        "/api/v4/projects/grp%2Fapp/merge_requests/7/discussions"
    );
    assert_eq!(
        reqs[0].json(),
        json!({
            "body": "Why a second B?",
            "position": {
                "position_type": "text",
                "base_sha": "b0", "start_sha": "s0", "head_sha": "h0",
                "old_path": "src/App.java", "new_path": "src/App.java",
                "new_line": 11
            }
        })
    );
    // Posted comments are published at once: a retry sends only the rest.
    let saved = CommentStore::load(&path).unwrap();
    assert_eq!(saved.published.len(), 2);
    assert_eq!(saved.drafts.len(), 1);
}

// ----- GitHub ---------------------------------------------------------------

fn github(addr: &str) -> crate::forge::github::Github {
    crate::forge::github::Github::new(
        "github.test",
        addr,
        &format!("{addr}/graphql"),
        "tok".into(),
        None,
    )
    .unwrap()
}

const NO_PENDING: &str = r#"{"data":{"repository":{"pullRequest":{"reviews":{"nodes":[]}}}}}"#;

fn thread_created(id: u64) -> fake::Response {
    ok(&format!(
        r#"{{"data":{{"addPullRequestReviewThread":{{"thread":{{"comments":{{"nodes":[{{"databaseId":{id},"body":""}}]}}}}}}}}}}"#
    ))
}

fn review_updated(body: &str) -> fake::Response {
    let v = json!({"data": {"updatePullRequestReview": {"pullRequestReview": {"databaseId": 55, "body": body}}}});
    ok(&v.to_string())
}

#[test]
fn plans_one_github_review() {
    let d = diff();
    let mut store = review_store(&d);
    store.add(
        Anchor::Reply {
            discussion: "PRRT_abc".into(),
            author: "alice".into(),
        },
        "Agreed, fixed.",
    );
    store.add(
        Anchor::Reply {
            discussion: "general:IC_1".into(),
            author: "bob".into(),
        },
        "Will do.",
    );
    let p = plan(&store, &d, &shas(), &Capabilities::github("github.com"));
    assert_eq!((p.forge, p.mode), (ForgeKind::Github, Mode::Drafts));
    let reqs = p.requests("o/r", "7");
    let paths: Vec<&str> = reqs.iter().map(|r| r.path.as_str()).collect();
    assert_eq!(
        paths,
        [
            "/repos/o/r/pulls/7/reviews",
            "/graphql",
            "/graphql",
            "/graphql",
            "/graphql",
            "/graphql",
            "/graphql",
            "/repos/o/r/pulls/7/reviews/:review/events",
        ]
    );
    assert_eq!(reqs[0].body, Some(json!({"commit_id": "h0"})));
    assert_eq!(
        reqs[1].body,
        Some(json!({"mutation": "addPullRequestReviewThread", "input": {
            "pullRequestReviewId": ":review", "body": "Why a second B?",
            "path": "src/App.java", "subjectType": "LINE", "line": 11, "side": "RIGHT"}}))
    );
    assert_eq!(
        reqs[2].body.as_ref().unwrap()["input"],
        json!({"pullRequestReviewId": ":review", "body": "Renamed on purpose?",
               "path": "src/New.java", "subjectType": "LINE",
               "line": 2, "side": "LEFT", "startLine": 1, "startSide": "LEFT"})
    );
    assert_eq!(
        reqs[3].body.as_ref().unwrap()["input"],
        json!({"pullRequestReviewId": ":review", "body": "Deleting this is fine.",
               "path": "src/Gone.java", "subjectType": "FILE"})
    );
    assert_eq!(
        reqs[4].body,
        Some(
            json!({"mutation": "addPullRequestReviewThreadReply", "input": {
            "pullRequestReviewId": ":review", "pullRequestReviewThreadId": "PRRT_abc",
            "body": "Agreed, fixed."}})
        )
    );
    // A reply to a general comment: in the review body, mentioning its author.
    assert_eq!(
        reqs[5].body,
        Some(json!({"mutation": "updatePullRequestReview", "input": {
            "pullRequestReviewId": ":review", "body": "@bob Will do."}}))
    );
    assert_eq!(
        reqs[6].body.as_ref().unwrap()["mutation"],
        "updatePullRequestReview"
    );
    assert_eq!(reqs[7].body, Some(json!({"event": "COMMENT"})));

    // Without drafts (never the case on GitHub, but supported): right away.
    let mut direct = p.clone();
    direct.mode = Mode::Direct;
    let paths: Vec<String> = direct
        .requests("o/r", "7")
        .into_iter()
        .map(|r| r.path)
        .collect();
    assert_eq!(
        paths,
        [
            "/repos/o/r/pulls/7/comments",
            "/repos/o/r/pulls/7/comments",
            "/repos/o/r/pulls/7/comments",
            "/graphql",
            "/repos/o/r/issues/7/comments",
            "/repos/o/r/issues/7/comments",
        ]
    );
}

#[test]
fn publishes_one_github_review() {
    let d = diff();
    let dir = tempfile::tempdir().unwrap();
    let path = comments_path(dir.path(), "mr-7");
    let mut store = review_store(&d);
    store.save(&path).unwrap();
    let p = plan(&store, &d, &shas(), &Capabilities::github("github.com"));

    let (addr, rx) = fake::serve(vec![
        ok(NO_PENDING),
        ok(r#"{"id":55,"node_id":"PRR_55","state":"PENDING","body":"","user":{"login":"me"}}"#),
        thread_created(201),
        thread_created(202),
        thread_created(203),
        review_updated("Looks consistent overall."),
        ok(
            r#"{"id":55,"node_id":"PRR_55","state":"COMMENTED","body":"Looks consistent overall."}"#,
        ),
    ]);
    let n = publish(&github(&addr), "o/r", 7, &p, &mut store, &path, &mut |_| {}).unwrap();
    assert_eq!(n, 4);

    let reqs: Vec<fake::Request> = rx.try_iter().collect();
    assert_eq!(reqs.len(), 7);
    assert!(reqs.iter().all(|r| r.authorization == "Bearer tok"));
    assert!(
        reqs[0].json()["query"]
            .as_str()
            .unwrap()
            .contains("PENDING")
    );
    assert_eq!(
        (reqs[1].path.as_str(), reqs[1].json()),
        ("/repos/o/r/pulls/7/reviews", json!({"commit_id": "h0"}))
    );
    let input = reqs[2].json()["variables"]["input"].clone();
    assert_eq!(
        input,
        json!({"pullRequestReviewId": "PRR_55", "body": "Why a second B?",
               "path": "src/App.java", "subjectType": "LINE", "line": 11, "side": "RIGHT"})
    );
    assert!(
        reqs[2].json()["query"]
            .as_str()
            .unwrap()
            .contains("addPullRequestReviewThread")
    );
    assert_eq!(reqs[4].json()["variables"]["input"]["subjectType"], "FILE");
    assert_eq!(
        reqs[5].json()["variables"]["input"],
        json!({"pullRequestReviewId": "PRR_55", "body": "Looks consistent overall."})
    );
    assert_eq!(
        (reqs[6].path.as_str(), reqs[6].json()),
        (
            "/repos/o/r/pulls/7/reviews/55/events",
            json!({"event": "COMMENT", "body": "Looks consistent overall."})
        )
    );

    let saved = CommentStore::load(&path).unwrap();
    assert!(saved.drafts.is_empty() && saved.summary.is_empty());
    assert_eq!(saved.published.len(), 3);
}

#[test]
fn an_interrupted_github_review_resumes_on_the_pending_review() {
    let d = diff();
    let dir = tempfile::tempdir().unwrap();
    let path = comments_path(dir.path(), "mr-7");
    let mut store = review_store(&d);
    let caps = Capabilities::github("github.com");
    let p = plan(&store, &d, &shas(), &caps);
    let (addr, _rx) = fake::serve(vec![
        ok(NO_PENDING),
        ok(r#"{"id":55,"node_id":"PRR_55","state":"PENDING","body":null}"#),
        thread_created(201),
        status("502 Bad Gateway", "oops"),
    ]);
    let err = publish(&github(&addr), "o/r", 7, &p, &mut store, &path, &mut |_| {}).unwrap_err();
    assert!(err.to_string().contains("after 1 of 4"), "{err}");
    let mut store = CommentStore::load(&path).unwrap();
    assert_eq!(store.drafts[0].remote_id, Some(201));

    // Again: the pending review is found, not created; the first thread is
    // not sent twice; a body written in the browser is kept under the summary.
    let p = plan(&store, &d, &shas(), &caps);
    let pending = r#"{"data":{"repository":{"pullRequest":{"reviews":{"nodes":[
        {"id":"PRR_55","databaseId":55,"body":"From the browser.",
         "comments":{"nodes":[{"databaseId":201,"body":"Why a second B?"}]}}]}}}}}"#;
    let (addr, rx) = fake::serve(vec![
        ok(pending),
        thread_created(202),
        thread_created(203),
        review_updated("Looks consistent overall.\n\nFrom the browser."),
        ok(r#"{"id":55,"node_id":"PRR_55","state":"COMMENTED"}"#),
    ]);
    publish(&github(&addr), "o/r", 7, &p, &mut store, &path, &mut |_| {}).unwrap();
    let reqs: Vec<fake::Request> = rx.try_iter().collect();
    assert_eq!(reqs.len(), 5);
    assert_eq!(
        reqs[1].json()["variables"]["input"]["body"],
        "Renamed on purpose?"
    );
    assert_eq!(
        reqs[3].json()["variables"]["input"]["body"],
        "Looks consistent overall.\n\nFrom the browser."
    );
    assert_eq!(
        reqs[4].json(),
        json!({"event": "COMMENT", "body": "Looks consistent overall.\n\nFrom the browser."})
    );
    assert!(store.drafts.is_empty());
}
