use std::sync::Mutex;

use super::*;
use crate::llm::LlmError;

type Respond = dyn Fn(&str, usize) -> Result<String, LlmError> + Sync;

/// Records prompts and answers with `respond(prompt, call_index)`.
struct Fake {
    respond: Box<Respond>,
    prompts: Mutex<Vec<String>>,
}

impl Fake {
    fn new(respond: impl Fn(&str, usize) -> Result<String, LlmError> + Sync + 'static) -> Self {
        Self {
            respond: Box::new(respond),
            prompts: Mutex::new(Vec::new()),
        }
    }

    /// Answers the given texts in turn.
    fn scripted(answers: &[&str]) -> Self {
        let answers: Vec<String> = answers.iter().map(|a| a.to_string()).collect();
        Self::new(move |_, i| Ok(answers[i.min(answers.len() - 1)].clone()))
    }

    fn calls(&self) -> usize {
        self.prompts.lock().unwrap().len()
    }

    fn prompt(&self, i: usize) -> String {
        self.prompts.lock().unwrap()[i].clone()
    }
}

impl LlmProvider for Fake {
    fn complete(&self, req: &LlmRequest) -> crate::llm::Result<String> {
        let i = {
            let mut p = self.prompts.lock().unwrap();
            p.push(req.prompt.clone());
            p.len() - 1
        };
        (self.respond)(&req.prompt, i)
    }
}

/// Hunk ids listed in a group prompt.
fn prompt_ids(prompt: &str) -> Vec<usize> {
    prompt
        .lines()
        .filter_map(|l| l.strip_prefix('[')?.split_once(']')?.0.parse().ok())
        .collect()
}

/// A diff with one hunk per `(path, changed line)`.
fn diff_of(hunks: &[(&str, &str)]) -> Diff {
    let mut raw = String::new();
    let mut last = "";
    for (i, (path, line)) in hunks.iter().enumerate() {
        if *path != last {
            raw.push_str(&format!(
                "diff --git a/{path} b/{path}\n--- a/{path}\n+++ b/{path}\n"
            ));
            last = path;
        }
        let at = i * 10 + 1;
        raw.push_str(&format!("@@ -{at},1 +{at},1 @@\n-old {i}\n+{line}\n"));
    }
    let mut d = crate::diff::parse(raw.as_bytes()).unwrap();
    crate::mechanical::mark(&mut d, &crate::mechanical::globset(&[]).unwrap());
    d
}

fn params(max: usize) -> Params {
    Params {
        model: Some("fast".into()),
        effort: None,
        max_prompt_chars: max,
        instructions: None,
        language: "English".into(),
        cwd: ".".into(),
    }
}

fn run(diff: &Diff, llm: &Fake, max: usize) -> Grouping {
    build(diff, &params(max), llm, &mut |_| {})
}

/// Every hunk in exactly one group, hunk lists consistent with layers,
/// hunkless files in the mechanical group, groups sorted by order.
fn assert_invariants(g: &Grouping, diff: &Diff) {
    let mut seen = vec![0; diff.hunks.len()];
    for (i, group) in g.groups.iter().enumerate() {
        assert_eq!(group.order, i);
        let mut from_layers: Vec<usize> = group
            .layers
            .iter()
            .flat_map(|l| l.hunk_ids.clone())
            .collect();
        from_layers.sort_unstable();
        assert_eq!(from_layers, group.hunk_ids, "group {}", group.title);
        for &h in &group.hunk_ids {
            seen[h] += 1;
        }
    }
    assert!(seen.iter().all(|&n| n == 1), "hunk coverage {seen:?}");
    let files: Vec<usize> = crate::mechanical::hunkless_files(diff).collect();
    let mech: Vec<usize> = g
        .groups
        .iter()
        .filter(|g| g.mechanical)
        .flat_map(|g| g.file_ids.clone())
        .collect();
    assert_eq!(mech, files);
}

const FOUR: &[(&str, &str)] = &[
    ("src/order/Order.java", "class Order {}"),
    ("src/order/OrderService.java", "void create() {}"),
    ("src/order/OrderController.java", "@PostMapping"),
    ("src/test/OrderTest.java", "@Test void create() {}"),
];

const VALID: &str = r#"{"groups":[
  {"title":"Order creation","summary":"Orders can be created.","layers":[
    {"name":"api","hunks":[2]},{"name":"service","hunks":[1]},{"name":"model","hunks":[0]}]},
  {"title":"Order tests","summary":"Tests.","layers":[{"name":"tests","hunks":[3]}]}]}"#;

#[test]
fn valid_answer() {
    let d = diff_of(FOUR);
    let llm = Fake::scripted(&[VALID]);
    let g = run(&d, &llm, 100_000);
    assert_eq!(g.source, Source::Llm);
    assert_eq!(llm.calls(), 1);
    assert_eq!(g.llm_calls, 1);
    assert!(g.warnings.is_empty(), "{:?}", g.warnings);
    assert_eq!(g.groups.len(), 2);
    assert_eq!(g.groups[0].title, "Order creation");
    assert_eq!(g.groups[0].hunk_ids, vec![0, 1, 2]);
    assert_eq!(g.model.as_deref(), Some("fast"));
    assert_eq!(g.prompt_version, PROMPT_VERSION);
    assert_invariants(&g, &d);

    let p = llm.prompt(0);
    assert_eq!(prompt_ids(&p), vec![0, 1, 2, 3]);
    assert!(p.contains("file src/order/Order.java (modified, java)"));
    assert!(p.contains("There are 4 hunks"));
}

#[test]
fn fenced_answer() {
    let d = diff_of(FOUR);
    let fenced = format!("Here is the grouping:\n```json\n{VALID}\n```\n");
    let llm = Fake::scripted(&[&fenced]);
    let g = run(&d, &llm, 100_000);
    assert_eq!(g.source, Source::Llm);
    assert_eq!(g.groups.len(), 2);
}

#[test]
fn invariant_violation_is_retried_with_the_error() {
    let d = diff_of(FOUR);
    let bad = r#"{"groups":[{"title":"All","summary":"s","layers":[{"name":"code","hunks":[0,1,1,9]}]}]}"#;
    let llm = Fake::scripted(&[bad, VALID]);
    let g = run(&d, &llm, 100_000);
    assert_eq!(g.source, Source::Llm);
    assert_eq!(llm.calls(), 2);
    let retry = llm.prompt(1);
    assert!(retry.contains("# Correction"));
    assert!(retry.contains(bad));
    assert!(retry.contains("unknown hunk ids: 9"));
    assert!(retry.contains("hunk ids placed more than once: 1"));
    assert!(retry.contains("missing hunk ids: 2, 3"));
    assert_eq!(g.warnings.len(), 1);
    assert_invariants(&g, &d);
}

#[test]
fn garbage_falls_back_to_directories() {
    let d = diff_of(FOUR);
    let llm = Fake::scripted(&["I cannot do that.", "{\"groups\": 3}"]);
    let g = run(&d, &llm, 100_000);
    assert_eq!(g.source, Source::Fallback);
    assert_eq!(llm.calls(), 2);
    let titles: Vec<_> = g.groups.iter().map(|g| g.title.as_str()).collect();
    assert_eq!(titles, ["Changes in src/"]);
    let layers: Vec<_> = g.groups[0].layers.iter().map(|l| l.name.as_str()).collect();
    assert_eq!(layers, ["core", "service", "api", "tests"]);
    assert!(g.warnings.iter().any(|w| w.contains("falling back")));
    assert_invariants(&g, &d);
}

#[test]
fn provider_error_falls_back_without_retry() {
    let d = diff_of(FOUR);
    let llm = Fake::new(|_, _| Err(LlmError::Reported("Not logged in".into())));
    let g = run(&d, &llm, 100_000);
    assert_eq!(g.source, Source::Fallback);
    assert_eq!(llm.calls(), 1);
    assert!(g.warnings[0].contains("Not logged in"));
    assert_invariants(&g, &d);
}

#[test]
fn partial_answer_keeps_valid_groups() {
    let d = diff_of(FOUR);
    let partial = r#"{"groups":[{"title":"Order creation","summary":"s","layers":[
        {"name":"model","hunks":[0]},{"name":"service","hunks":[1, 42]}]}]}"#;
    let llm = Fake::scripted(&[partial, partial]);
    let g = run(&d, &llm, 100_000);
    assert_eq!(g.source, Source::Partial);
    assert_eq!(llm.calls(), 2);
    let titles: Vec<_> = g.groups.iter().map(|g| g.title.as_str()).collect();
    assert_eq!(titles, ["Order creation", "Changes in src/"]);
    assert_eq!(g.groups[0].hunk_ids, vec![0, 1]);
    assert!(g.warnings.iter().any(|w| w.contains("2 hunk(s) left out")));
    assert_invariants(&g, &d);
}

#[test]
fn big_reviews_are_chunked_by_module_then_merged() {
    let hunks: Vec<(String, String)> = (0..30)
        .map(|i| {
            let module = ["back", "front", "infra"][i / 10];
            (
                format!("{module}/pkg{}/File{i}.java", i % 2),
                format!("int value{i} = compute{i}(firstArgument, secondArgument, third);"),
            )
        })
        .collect();
    let refs: Vec<(&str, &str)> = hunks
        .iter()
        .map(|(p, l)| (p.as_str(), l.as_str()))
        .collect();
    let d = diff_of(&refs);

    // One group per chunk, titled after its first file; the merge joins
    // the first and last groups.
    let llm = Fake::new(|prompt, _| {
        if prompt.contains("Decide which groups to merge") {
            return Ok(r#"{"merges":[{"groups":[0,"g2"],"title":"Merged","summary":"m"}]}"#.into());
        }
        let ids = prompt_ids(prompt);
        let title = prompt
            .lines()
            .find_map(|l| l.strip_prefix("file "))
            .unwrap()
            .to_string();
        Ok(
            serde_json::json!({"groups":[{"title": title, "summary": "s",
            "layers":[{"name":"service","hunks": ids}]}]})
            .to_string(),
        )
    });
    let max = group_prompt_size_for(&d, 12);
    let g = run(&d, &llm, max);
    assert_eq!(g.source, Source::Llm, "{:?}", g.warnings);
    let chunks = llm.calls() - 1;
    assert!(chunks >= 3, "only {chunks} chunk(s)");
    assert_eq!(g.llm_calls, chunks + 1);
    assert_eq!(g.groups.len(), chunks - 1);
    assert_eq!(g.groups[0].title, "Merged");
    assert_invariants(&g, &d);

    // Chunks never mix top-level modules when they fit separately.
    for i in 0..llm.calls() {
        let p = llm.prompt(i);
        if p.contains("Decide which groups to merge") {
            assert!(p.contains("[0] "));
            continue;
        }
        let modules: BTreeSet<&str> = p
            .lines()
            .filter_map(|l| l.strip_prefix("file ")?.split('/').next())
            .collect();
        assert_eq!(modules.len(), 1, "{modules:?}");
        assert!(p.len() <= max, "prompt of {} > {max}", p.len());
    }
}

/// A budget fitting about `hunks` compressed hunks.
fn group_prompt_size_for(d: &Diff, hunks: usize) -> usize {
    let all: Vec<usize> = (0..d.hunks.len()).collect();
    let blocks = prompt::blocks(d, &all, usize::MAX);
    let per_hunk = blocks.iter().map(|b| b.text.len()).sum::<usize>() / d.hunks.len();
    prompt::group_overhead(None, "English") + per_hunk * hunks
}

#[test]
fn failed_merge_keeps_chunk_groups() {
    let refs: Vec<(String, String)> = (0..20)
        .map(|i| {
            (
                format!("m{}/F{i}.kt", i / 10),
                format!("val x{i} = compute{i}(firstArgument, secondArgument, third)"),
            )
        })
        .collect();
    let refs: Vec<(&str, &str)> = refs.iter().map(|(p, l)| (p.as_str(), l.as_str())).collect();
    let d = diff_of(&refs);
    let llm = Fake::new(|prompt, _| {
        if prompt.contains("Decide which groups to merge") {
            return Ok(r#"{"merges":[{"groups":[0,99],"title":"x"}]}"#.into());
        }
        Ok(serde_json::json!({"groups":[{"title":"t","summary":"s",
            "layers":[{"name":"code","hunks": prompt_ids(prompt)}]}]})
        .to_string())
    });
    let g = run(&d, &llm, group_prompt_size_for(&d, 10));
    assert_eq!(g.source, Source::Llm);
    // Two merge attempts; each chunk keeps its own group.
    let chunks = llm.calls() - 2;
    assert!(chunks >= 2);
    assert_eq!(g.groups.len(), chunks);
    assert!(g.warnings.iter().any(|w| w.contains("left unmerged")));
    assert_invariants(&g, &d);
}

#[test]
fn mechanical_changes_skip_the_llm() {
    let raw = "diff --git a/package-lock.json b/package-lock.json\n--- a/package-lock.json\n\
        +++ b/package-lock.json\n@@ -1 +1 @@\n-1\n+2\n\
        diff --git a/src/A.java b/src/A.java\n--- a/src/A.java\n+++ b/src/A.java\n\
        @@ -1,1 +1,2 @@\n-if (a) { b(); }\n+if (a) {\n+  b(); }\n\
        diff --git a/old/B.java b/new/B.java\nsimilarity index 100%\nrename from old/B.java\nrename to new/B.java\n\
        diff --git a/logo.png b/logo.png\nBinary files a/logo.png and b/logo.png differ\n";
    let mut d = crate::diff::parse(raw.as_bytes()).unwrap();
    crate::mechanical::mark(&mut d, &crate::mechanical::globset(&[]).unwrap());
    let llm = Fake::scripted(&["unused"]);
    let g = run(&d, &llm, 100_000);
    assert_eq!(llm.calls(), 0);
    assert_eq!(g.source, Source::Mechanical);
    assert_eq!(g.groups.len(), 1);
    let m = &g.groups[0];
    assert!(m.mechanical);
    let layers: Vec<_> = m
        .layers
        .iter()
        .map(|l| (l.name.as_str(), l.hunk_ids.clone()))
        .collect();
    assert_eq!(layers, [("generated", vec![0]), ("formatting", vec![1])]);
    assert_eq!(m.file_ids.len(), 2);
    assert!(
        m.summary.starts_with("1 hunk(s) in lockfiles"),
        "{}",
        m.summary
    );
    assert_invariants(&g, &d);
}

#[test]
fn mechanical_group_comes_last_and_is_left_out_of_the_prompt() {
    let mut hunks = FOUR.to_vec();
    hunks.push(("yarn.lock", "resolved"));
    let d = diff_of(&hunks);
    let llm = Fake::scripted(&[VALID]);
    let g = run(&d, &llm, 100_000);
    assert_eq!(g.source, Source::Llm);
    assert!(!llm.prompt(0).contains("yarn.lock"));
    let last = g.groups.last().unwrap();
    assert!(last.mechanical);
    assert_eq!(last.hunk_ids, vec![4]);
    assert_invariants(&g, &d);
}

#[test]
fn group_progress_uses_review_state() {
    let raw = "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-x\n+y\n\
        diff --git a/Cargo.lock b/Cargo.lock\n--- a/Cargo.lock\n+++ b/Cargo.lock\n@@ -1 +1 @@\n-1\n+2\n\
        diff --git a/x b/y\nsimilarity index 100%\nrename from x\nrename to y\n";
    let mut d = crate::diff::parse(raw.as_bytes()).unwrap();
    crate::mechanical::mark(&mut d, &crate::mechanical::globset(&[]).unwrap());
    let llm = Fake::scripted(&[
        r#"{"groups":[{"title":"A","summary":"s","layers":[{"name":"code","hunks":[0]}]}]}"#,
    ]);
    let g = run(&d, &llm, 100_000);
    let mut state = ReviewState::default();
    let mech = &g.groups[1];
    assert_eq!(mech.progress(&d, &state), (0, 2));
    mech.set_reviewed(&d, &mut state, true);
    assert!(mech.is_reviewed(&d, &state));
    assert_eq!(g.progress(&d, &state), (2, 3));
    assert_eq!(state.progress(&d), (2, 3));
    assert_eq!(g.group_of_hunk(1), Some(1));
}

#[test]
fn cache_is_reused_on_reopen() {
    use crate::config::Config;
    use crate::git::testutil::*;
    use crate::review::{self, Target};

    let tmp = tempfile::tempdir().unwrap();
    let g = repo(tmp.path());
    commit(&g, &[("src/a.rs", "1\n"), ("src/b.rs", "1\n")], "base");
    commit(&g, &[("src/a.rs", "2\n"), ("src/b.rs", "2\n")], "head");
    let r = review::open(
        &g,
        &Config::default(),
        &Target::parse(Some("HEAD~1..HEAD")).unwrap(),
        |_| {},
    )
    .unwrap();
    let answer =
        r#"{"groups":[{"title":"A","summary":"s","layers":[{"name":"code","hunks":[0,1]}]}]}"#;
    let cfg = Config::default();

    let llm = Fake::scripted(&[answer]);
    let first = review::group(&r, &cfg, &llm, |_| {}, true).unwrap();
    assert!(!first.from_cache);
    assert_eq!(llm.calls(), 1);
    let path = cache_path(&g.survol_dir().unwrap(), &r.head_sha);
    assert!(path.is_file());

    let llm = Fake::scripted(&[answer]);
    let mut messages = Vec::new();
    let second = review::group(&r, &cfg, &llm, |m| messages.push(m.to_string()), true).unwrap();
    assert_eq!(llm.calls(), 0);
    assert!(second.from_cache);
    assert_eq!(second.groups, first.groups);
    assert_eq!(messages, ["groups loaded from cache"]);

    // `--no-cache` recomputes; a different model misses the cache.
    let llm = Fake::scripted(&[answer]);
    review::group(&r, &cfg, &llm, |_| {}, false).unwrap();
    assert_eq!(llm.calls(), 1);
    let mut other = cfg.clone();
    other.llm.group_model = Some("other".into());
    let llm = Fake::scripted(&[answer]);
    review::group(&r, &other, &llm, |_| {}, true).unwrap();
    assert_eq!(llm.calls(), 1);

    // Switching language regenerates instead of returning English text.
    let mut french = cfg.clone();
    french.llm.override_language(Some("fr"));
    let llm = Fake::scripted(&[answer]);
    review::group(&r, &french, &llm, |_| {}, true).unwrap();
    assert_eq!(llm.calls(), 1);
    assert!(llm.prompt(0).contains("in French."));

    // Project instructions reach the prompt and are part of the key.
    std::fs::create_dir_all(tmp.path().join(".survol")).unwrap();
    std::fs::write(
        review::instructions_path(tmp.path()),
        "Hexagonal: domain, ports, adapters.",
    )
    .unwrap();
    let llm = Fake::scripted(&[answer]);
    review::group(&r, &cfg, &llm, |_| {}, true).unwrap();
    assert_eq!(llm.calls(), 1);
    assert!(
        llm.prompt(0)
            .contains("Hexagonal: domain, ports, adapters.")
    );
}

#[test]
fn fallback_is_not_cached() {
    use crate::config::Config;
    use crate::git::testutil::*;
    use crate::review::{self, Target};

    let tmp = tempfile::tempdir().unwrap();
    let g = repo(tmp.path());
    commit(&g, &[("a.rs", "1\n")], "base");
    commit(&g, &[("a.rs", "2\n")], "head");
    let r = review::open(
        &g,
        &Config::default(),
        &Target::parse(Some("HEAD~1..")).unwrap(),
        |_| {},
    )
    .unwrap();
    let llm = Fake::scripted(&["nope"]);
    let first = review::group(&r, &Config::default(), &llm, |_| {}, true).unwrap();
    assert_eq!(first.source, Source::Fallback);
    assert!(!cache_path(&g.survol_dir().unwrap(), &r.head_sha).exists());
}

#[test]
fn disabled_llm_groups_by_directory_without_calling_it() {
    use crate::config::Config;
    use crate::git::testutil::*;
    use crate::review::{self, Target};

    let tmp = tempfile::tempdir().unwrap();
    let g = repo(tmp.path());
    commit(&g, &[("src/a.rs", "1\n"), ("Cargo.lock", "1\n")], "base");
    commit(&g, &[("src/a.rs", "2\n"), ("Cargo.lock", "2\n")], "head");
    let r = review::open(
        &g,
        &Config::default(),
        &Target::parse(Some("HEAD~1..")).unwrap(),
        |_| {},
    )
    .unwrap();
    let mut cfg = Config::default();
    cfg.llm.enabled = false;
    let llm = Fake::scripted(&["unused"]);
    let grouping = review::group(&r, &cfg, &llm, |_| {}, true).unwrap();
    assert_eq!(llm.calls(), 0);
    assert_eq!(grouping.source, Source::Fallback);
    assert!(grouping.warnings.is_empty());
    assert_invariants(&grouping, &r.diff);
    assert!(grouping.groups.last().unwrap().mechanical);
    assert!(!cache_path(&g.survol_dir().unwrap(), &r.head_sha).exists());
}

#[test]
fn cache_key_depends_on_language() {
    let d = diff_of(&[("src/a.rs", "a")]);
    let en = params(10_000);
    let fr = Params {
        language: "French".into(),
        ..en.clone()
    };
    assert_ne!(cache_key(&d, &en), cache_key(&d, &fr));
    assert_eq!(cache_key(&d, &en), cache_key(&d, &params(10_000)));
}

#[test]
fn offline_texts_follow_the_language() {
    let d = diff_of(&[("src/a.rs", "a"), ("b.rs", "b")]);
    let fr = Params {
        language: "French".into(),
        ..params(10_000)
    };
    let titles: Vec<String> = build_offline(&d, &fr)
        .groups
        .into_iter()
        .map(|g| g.title)
        .collect();
    assert_eq!(
        titles,
        [
            "Modifications à la racine du dépôt",
            "Modifications dans src/"
        ]
    );
    let titles: Vec<String> = build_offline(&d, &params(10_000))
        .groups
        .into_iter()
        .map(|g| g.title)
        .collect();
    assert_eq!(
        titles,
        ["Changes at the repository root", "Changes in src/"]
    );
}
