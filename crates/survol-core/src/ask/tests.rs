use std::collections::HashMap;
use std::sync::Mutex;

use super::*;
use crate::graph::{self, Lang};
use crate::group::{Group, Layer, Source};
use crate::index::{Index, parse_file};

const SERVICE: &str = "package app.owner;
public class OwnerService {
    private final OwnerRepository repo;
    OwnerService(OwnerRepository repo) { this.repo = repo; }
    public Owner find(int id) {
        return repo.findById(id);
    }
}
";
const REPO: &str = "package app.owner;
public interface OwnerRepository {
    Owner findById(int id);
}
";
const CONTROLLER: &str = "package app.web;
import app.owner.OwnerService;
public class OwnerController {
    private final OwnerService service;
    OwnerController(OwnerService service) { this.service = service; }
    Object show(int id) { return service.find(id); }
}
";
const TEST: &str = "package app.owner;
class OwnerServiceTest {
    OwnerService service;
    void findsOwner() { service.find(1); }
}
";
const PATH: &str = "src/main/java/app/owner/OwnerService.java";
const CTRL: &str = "src/main/java/app/web/OwnerController.java";

struct Fixture {
    diff: Diff,
    graph: Graph,
    grouping: Grouping,
    files: HashMap<(String, String), String>,
}

fn fixture() -> Fixture {
    let raw = format!(
        "diff --git a/{PATH} b/{PATH}\n--- a/{PATH}\n+++ b/{PATH}\n\
         @@ -6,1 +6,1 @@\n-        return repo.findById(0);\n+        return repo.findById(id);\n"
    );
    let diff = crate::diff::parse(raw.as_bytes()).unwrap();
    let sources = [
        (PATH, SERVICE),
        ("src/main/java/app/owner/OwnerRepository.java", REPO),
        (CTRL, CONTROLLER),
        ("src/test/java/app/owner/OwnerServiceTest.java", TEST),
    ];
    let mut files: Vec<_> = sources
        .iter()
        .map(|(p, s)| parse_file(p, Lang::from_path(p).unwrap(), s))
        .collect();
    files.sort_by(|a, b| a.path.cmp(&b.path));
    let index = Index {
        files,
        base_files: Vec::new(),
        stats: Default::default(),
    };
    let graph = graph::build(&index, &diff, &graph::default_rules());
    let grouping = Grouping {
        groups: vec![Group {
            id: 7,
            title: "Owner lookup".into(),
            summary: "Finds owners by their id.".into(),
            layers: vec![Layer {
                name: "service".into(),
                hunk_ids: vec![0],
            }],
            order: 0,
            hunk_ids: vec![0],
            file_ids: vec![],
            mechanical: false,
        }],
        source: Source::Llm,
        model: None,
        prompt_version: 0,
        key: String::new(),
        from_cache: false,
        llm_calls: 1,
        warnings: vec![],
    };
    let mut blobs = HashMap::new();
    for (p, s) in sources {
        blobs.insert(("head".to_string(), p.to_string()), s.to_string());
    }
    blobs.insert(
        ("base".to_string(), PATH.to_string()),
        SERVICE.replace("findById(id)", "findById(0)"),
    );
    Fixture {
        diff,
        graph,
        grouping,
        files: blobs,
    }
}

fn prompt(f: &Fixture, subject: &Subject, question: &str) -> Prompt {
    let read = |rev: &str, path: &str| f.files.get(&(rev.to_string(), path.to_string())).cloned();
    let src = Sources {
        diff: &f.diff,
        graph: Some(&f.graph),
        grouping: Some(&f.grouping),
        read: &read,
        base_sha: "base",
        head_sha: "head",
    };
    build_prompt(
        &src,
        subject,
        question,
        Some("Hexagonal architecture."),
        "French",
    )
    .unwrap()
}

fn find_id(f: &Fixture) -> String {
    let s = f.graph.find("OwnerService.find")[0];
    f.graph.symbol(s).id.clone()
}

#[test]
fn symbol_prompt_has_code_links_group_and_rules() {
    let f = fixture();
    let p = prompt(
        &f,
        &Subject::Symbol(find_id(&f)),
        "What does this component do?",
    );
    let t = &p.text;
    assert_eq!(p.label, "method OwnerService.find");
    assert!(
        t.contains("## Subject: method `OwnerService.find` at src/main/java/app/owner/OwnerService.java:5 (changed by this review)"),
        "{t}"
    );
    // Numbered code of the symbol, from the head blob.
    assert!(
        t.contains("    6 |         return repo.findById(id);"),
        "{t}"
    );
    // The hunk with both line numbers.
    assert!(
        t.contains("    6       | -        return repo.findById(0);"),
        "{t}"
    );
    assert!(
        t.contains("          6 | +        return repo.findById(id);"),
        "{t}"
    );
    // Caller at the line of the call, with code around it; the test apart.
    assert!(
        t.contains(
            "- `OwnerController.show` (method) at src/main/java/app/web/OwnerController.java:6"
        ),
        "{t}"
    );
    assert!(t.contains("in an untouched file"), "{t}");
    assert!(t.contains("    6 |     Object show(int id)"), "{t}");
    assert!(t.contains("## Tests exercising it (2)"), "{t}");
    assert!(t.contains("`OwnerServiceTest.findsOwner`"), "{t}");
    assert!(t.contains("Owner lookup"), "{t}");
    assert!(t.contains("Finds owners by their id."), "{t}");
    assert!(t.contains("Hexagonal architecture."));
    assert!(t.contains("Write the answer in French."));
    assert!(t.trim_end().ends_with("What does this component do?"));
    assert!(!t.contains("{{"));

    // Valid references: what was shown, on the right side.
    assert!(p.refs.contains(PATH, Side::New, 5));
    assert!(p.refs.contains(PATH, Side::Old, 6));
    assert!(p.refs.contains(CTRL, Side::New, 4));
    assert!(!p.refs.contains(CTRL, Side::New, 1));
}

#[test]
fn group_and_hunk_prompts() {
    let f = fixture();
    let p = prompt(&f, &Subject::Group(7), "How do the pieces fit together?");
    assert_eq!(p.label, "group 1. Owner lookup");
    assert!(
        p.text
            .contains("## Subject: group 1 of 1 in reading order: \"Owner lookup\"")
    );
    assert!(p.text.contains("Layers: service (1 hunks)"));
    assert!(p.text.contains("`OwnerService.find` (method) at src/main/java/app/owner/OwnerService.java:5: 1 caller(s), 1 in untouched files; called by `OwnerController.show` at src/main/java/app/web/OwnerController.java:6"), "{}", p.text);
    assert!(p.text.contains("#### layer service"));
    // A listed location is a valid reference even without code.
    assert!(p.refs.contains(CTRL, Side::New, 6));

    let hash = f.diff.hunks[0].content_hash.clone();
    let p = prompt(&f, &Subject::Hunk(hash), "Why?");
    assert_eq!(p.label, format!("hunk {PATH}:6"));
    assert!(p.text.contains("## Subject: a hunk of"));
    assert!(p.text.contains("# The symbol this hunk changes"));
    assert!(p.text.contains("## Called by"));

    let read = |_: &str, _: &str| None;
    let src = Sources {
        diff: &f.diff,
        graph: None,
        grouping: None,
        read: &read,
        base_sha: "b",
        head_sha: "h",
    };
    assert!(matches!(
        build_prompt(&src, &Subject::Symbol("x".into()), "q", None, "English"),
        Err(AskError::Subject(_))
    ));
    assert!(build_prompt(&src, &Subject::Group(7), "q", None, "English").is_err());
}

#[test]
fn parses_and_validates_references() {
    let mut refs = RefIndex::default();
    refs.add("src/a/Owner.java", Side::New, 10);
    refs.add("src/a/Owner.java", Side::New, 11);
    refs.add("src/a/Owner.java", Side::Old, 3);
    refs.add("src/b/Owner.java", Side::New, 1);
    refs.add("web/app.ts", Side::New, 4);
    let text = "It saves [src/a/Owner.java:10], then (see web/app.ts:4-6). \
        Removed: [src/a/Owner.java:3 (old)]. Invented: [src/a/Owner.java:99] \
        and [Nope.java:1]; ambiguous [Owner.java:1]; short [app.ts:4]. \
        Not refs: http://x.io:80 and 12:30 and a:b.";
    let r = parse_refs(text, &refs);
    let got: Vec<(&str, u32, Option<u32>, Side, bool)> = r
        .iter()
        .map(|c| (c.path.as_str(), c.line, c.end_line, c.side, c.valid))
        .collect();
    assert_eq!(
        got,
        [
            ("src/a/Owner.java", 10, None, Side::New, true),
            ("web/app.ts", 4, Some(6), Side::New, true),
            ("src/a/Owner.java", 3, None, Side::Old, true),
            ("src/a/Owner.java", 99, None, Side::New, false),
            ("Nope.java", 1, None, Side::New, false),
            ("Owner.java", 1, None, Side::New, false),
            ("web/app.ts", 4, None, Side::New, true),
        ]
    );
    // Offsets delimit the reference itself.
    assert_eq!(&text[r[0].start..r[0].end], "src/a/Owner.java:10");
    assert_eq!(&text[r[1].start..r[1].end], "web/app.ts:4-6");
    assert_eq!(r[6].raw_path, "app.ts");
}

struct Counting {
    calls: Mutex<usize>,
    answer: String,
}

impl LlmProvider for Counting {
    fn complete(&self, req: &LlmRequest) -> crate::llm::Result<String> {
        assert!(req.prompt.contains("# Question"));
        assert_eq!(req.model.as_deref(), Some("opus"));
        *self.calls.lock().unwrap() += 1;
        Ok(self.answer.clone())
    }
}

#[test]
fn asks_caches_and_keeps_history() {
    let f = fixture();
    let p = prompt(&f, &Subject::Symbol(find_id(&f)), "Who calls it?");
    let dir = tempfile::tempdir().unwrap();
    let llm = Counting {
        calls: Mutex::new(0),
        answer:
            "```\nOwnerController.show calls it [src/main/java/app/web/OwnerController.java:6], \
                 it reads the repository [OwnerService.java:6] and [Foo.java:3].\n```"
                .into(),
    };
    let params = Params {
        model: Some("opus".into()),
        cwd: dir.path().into(),
        cache_dir: Some(cache_dir(dir.path(), "head")),
        use_cache: true,
        head_sha: "head".into(),
    };
    let a = ask(&p, &llm, &params).unwrap();
    assert!(!a.from_cache);
    assert!(
        a.text.starts_with("OwnerController.show calls it"),
        "{}",
        a.text
    );
    assert_eq!(a.refs.len(), 3);
    assert!(a.refs[0].valid && a.refs[1].valid);
    assert_eq!(a.refs[1].path, PATH);
    assert_eq!(a.unknown_refs(), 1);

    let again = ask(&p, &llm, &params).unwrap();
    assert!(again.from_cache);
    assert_eq!(again.text, a.text);
    assert_eq!(*llm.calls.lock().unwrap(), 1);
    // Another question is another key.
    let other = prompt(&f, &Subject::Symbol(find_id(&f)), "How is it tested?");
    assert_ne!(cache_key(&other, Some("opus")), a.key);
    assert_ne!(cache_key(&p, Some("haiku")), a.key);
    let no_cache = Params {
        use_cache: false,
        ..params.clone()
    };
    ask(&p, &llm, &no_cache).unwrap();
    assert_eq!(*llm.calls.lock().unwrap(), 2);

    let h = history_path(dir.path(), "mr-1");
    append_history(&h, &a).unwrap();
    append_history(&h, &again).unwrap();
    let all = load_history(&h).unwrap();
    assert_eq!(all.len(), 1);
    assert!(!all[0].from_cache);

    let empty = Counting {
        calls: Mutex::new(0),
        answer: "  ".into(),
    };
    assert!(matches!(ask(&p, &empty, &no_cache), Err(AskError::Empty)));
}
