use std::collections::HashMap;
use std::io::{BufReader, Write};
use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::client::{read_message, write_frame};
use super::refine::{is_call, word_columns};
use super::*;
use crate::config::LspConfig;
use crate::graph::{EdgeKind, Graph, SymbolKind, build, default_rules};
use crate::index::{Index, parse_file};

// ---- transport

#[test]
fn frames_round_trip() {
    let mut buf = Vec::new();
    write_frame(&mut buf, &json!({"id": 1, "result": "é"})).unwrap();
    write_frame(&mut buf, &json!({"method": "x"})).unwrap();
    assert!(buf.starts_with(b"Content-Length: "));
    let mut r = BufReader::new(buf.as_slice());
    assert_eq!(
        read_message(&mut r).unwrap(),
        Some(json!({"id": 1, "result": "é"}))
    );
    assert_eq!(read_message(&mut r).unwrap(), Some(json!({"method": "x"})));
    assert_eq!(read_message(&mut r).unwrap(), None);
}

/// An in-process server: `handle` answers each request (`None`: never),
/// and may send notifications first.
fn fake_server(
    handle: impl Fn(&str, &Value) -> (Vec<Value>, Option<Value>) + Send + 'static,
) -> Client {
    let (client_read, mut server_write) = std::io::pipe().unwrap();
    let (server_read, client_write) = std::io::pipe().unwrap();
    std::thread::spawn(move || {
        let mut r = BufReader::new(server_read);
        while let Ok(Some(msg)) = read_message(&mut r) {
            let Some(method) = msg.get("method").and_then(Value::as_str) else {
                // A response to one of our requests: echo it as a notification.
                let _ = write_frame(
                    &mut server_write,
                    &json!({"jsonrpc": "2.0", "method": "echo", "params": msg}),
                );
                continue;
            };
            let params = msg.get("params").cloned().unwrap_or(Value::Null);
            let (notes, result) = handle(method, &params);
            for n in notes {
                let _ = write_frame(&mut server_write, &n);
            }
            if let (Some(id), Some(result)) = (msg.get("id"), result) {
                let _ = write_frame(
                    &mut server_write,
                    &json!({"jsonrpc": "2.0", "id": id, "result": result}),
                );
            }
            let _ = server_write.flush();
        }
    });
    Client::new(client_read, client_write)
}

fn note(method: &str, params: Value) -> Value {
    json!({"jsonrpc": "2.0", "method": method, "params": params})
}

#[test]
fn client_matches_responses_and_times_out() {
    let c = fake_server(|method, params| match method {
        "slow" => (vec![], None),
        "ask" => (
            // A server request comes first: answered by the client.
            vec![
                json!({"jsonrpc": "2.0", "id": 99, "method": "workspace/configuration",
                "params": {"items": [{}, {}]}}),
            ],
            Some(json!({"echo": params})),
        ),
        _ => (vec![note("hello", json!(1))], Some(Value::Null)),
    });
    let t = Duration::from_secs(5);
    assert_eq!(c.request("ask", json!(7), t).unwrap(), json!({"echo": 7}));
    // The client answered `workspace/configuration` with one null per item.
    let echo = c.next_notification(t).unwrap().unwrap();
    assert_eq!(echo.method, "echo");
    assert_eq!(echo.params["id"], 99);
    assert_eq!(echo.params["result"], json!([null, null]));

    let err = c
        .request("slow", Value::Null, Duration::from_millis(50))
        .unwrap_err();
    assert!(matches!(err, LspError::Timeout(m) if m == "slow"));
    c.request("other", Value::Null, t).unwrap();
    assert_eq!(c.next_notification(t).unwrap().unwrap().method, "hello");
}

#[test]
fn server_waits_for_readiness_and_maps_locations() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    let lib_uri = "jdt://contents/rt.jar/java.lang/String.class";
    let root_uri = server::file_uri(&root);
    let c = fake_server(move |method, _| match method {
        "initialize" => (vec![], Some(json!({"capabilities": {}}))),
        "initialized" => (
            vec![
                note(
                    "language/status",
                    json!({"type": "Starting", "message": "Init"}),
                ),
                note(
                    "$/progress",
                    json!({"token": "t", "value": {"kind": "begin", "title": "Importing", "percentage": 10}}),
                ),
                note(
                    "language/status",
                    json!({"type": "ServiceReady", "message": "Ready"}),
                ),
            ],
            None,
        ),
        "textDocument/definition" => (
            vec![],
            Some(json!([
                {"uri": format!("{root_uri}/src/A.java"), "range": {"start": {"line": 4, "character": 8}, "end": {"line": 4, "character": 9}}},
                {"targetUri": lib_uri, "targetRange": {"start": {"line": 0, "character": 0}},
                 "targetSelectionRange": {"start": {"line": 9, "character": 2}}},
            ])),
        ),
        "textDocument/references" => (vec![], Some(Value::Null)),
        _ => (vec![], Some(Value::Null)),
    });
    let mut s = Server::with_client(c, &root, Readiness::ServiceReady, Duration::from_secs(5));
    let deadline = Instant::now() + Duration::from_secs(5);
    s.initialize(Value::Null, deadline).unwrap();
    let mut seen = Vec::new();
    assert!(
        s.wait_ready(deadline, &mut |m| seen.push(m.to_string()))
            .unwrap()
    );
    assert_eq!(seen, ["Init", "Importing 10%"]);
    s.set_text("src/B.java", "class B {}".into());
    let locs = s.definition("src/B.java", 3, 4).unwrap();
    assert_eq!(
        locs,
        [
            Loc {
                path: Some("src/A.java".into()),
                line: 5,
                column: 8
            },
            Loc {
                path: None,
                line: 10,
                column: 2
            }
        ]
    );
    assert!(s.references("src/B.java", 1, 6).unwrap().is_empty());
}

#[test]
fn progress_readiness_waits_for_the_end_of_work() {
    let c = fake_server(|method, _| match method {
        "initialized" => (
            vec![
                note(
                    "$/progress",
                    json!({"token": 1, "value": {"kind": "begin", "title": "Indexing"}}),
                ),
                note(
                    "$/progress",
                    json!({"token": 1, "value": {"kind": "report", "message": "a.kt"}}),
                ),
                note("$/progress", json!({"token": 1, "value": {"kind": "end"}})),
            ],
            None,
        ),
        _ => (vec![], Some(Value::Null)),
    });
    let mut s = Server::with_client(
        c,
        Path::new("/x"),
        Readiness::Progress,
        Duration::from_secs(5),
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    s.initialize(Value::Null, deadline).unwrap();
    let mut seen = Vec::new();
    assert!(
        s.wait_ready(deadline, &mut |m| seen.push(m.to_string()))
            .unwrap()
    );
    assert_eq!(seen, ["Indexing", "a.kt"]);

    // Never ready before the deadline.
    let c = fake_server(|method, _| match method {
        "initialized" => (
            vec![note(
                "$/progress",
                json!({"token": 1, "value": {"kind": "begin", "title": "Indexing"}}),
            )],
            None,
        ),
        _ => (vec![], Some(Value::Null)),
    });
    let mut s = Server::with_client(
        c,
        Path::new("/x"),
        Readiness::Progress,
        Duration::from_secs(5),
    );
    s.initialize(Value::Null, deadline).unwrap();
    let soon = Instant::now() + Duration::from_millis(300);
    assert!(!s.wait_ready(soon, &mut |_| {}).unwrap());
}

// ---- text helpers

#[test]
fn finds_names_and_calls() {
    assert_eq!(word_columns("a.getPet(getPetId)", "getPet"), [2]);
    // UTF-16 columns.
    assert_eq!(word_columns("\"é😀\" + x.f(1)", "f"), [10]);
    assert!(is_call("o.getPet (1)", 2, "getPet"));
    assert!(is_call("list.<String>of()", 13, "of"));
    assert!(is_call("foo<A<B>>(x)", 0, "foo"));
    assert!(is_call("items.forEach { it }", 6, "forEach"));
    assert!(is_call("map(Owner::getPet)", 11, "getPet"));
    assert!(!is_call("import { getPet } from './x';", 9, "getPet"));
    assert!(!is_call("this.getPet = x", 5, "getPet"));
}

// ---- refinement

const OWNER: &str = "package app;\n\
public class Owner {\n\
    public Pet getPet(Integer id) {\n\
        return null;\n\
    }\n\
    public Pet getPet(String name) {\n\
        return null;\n\
    }\n\
}\n";

const CONTROLLER: &str = "package app;\n\
public class Controller {\n\
    Pet byId(Owner o) { return o.getPet(1); }\n\
    Pet byName(Owner o) { return o.getPet(\"x\"); }\n\
    int size(java.util.List<String> l) { return l.size(); }\n\
}\n";

const OTHER: &str = "package other;\n\
import app.Owner;\n\
public class Other {\n\
    Object go() { return lookup().getPet(3); }\n\
}\n";

/// Graph of files written under `root`, with `changed` marked as changed.
fn graph_on_disk(root: &Path, files: &[(&str, &str)], changed: &[&str]) -> Graph {
    let mut parsed: Vec<_> = files
        .iter()
        .map(|(p, s)| {
            let path = root.join(p);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, s).unwrap();
            parse_file(p, crate::index::Lang::from_path(p).unwrap(), s)
        })
        .collect();
    parsed.sort_by(|a, b| a.path.cmp(&b.path));
    let index = Index {
        files: parsed,
        base_files: Vec::new(),
        stats: Default::default(),
    };
    let g = build(&index, &Default::default(), &default_rules());
    let mut data = g.data().clone();
    for id in changed {
        let s = g.by_id(id).unwrap_or_else(|| panic!("{id}"));
        data.symbols[s as usize].changed = true;
    }
    Graph::new(data)
}

/// Answers from tables: `(path, line)` → locations.
#[derive(Default)]
struct Fake {
    defs: HashMap<(String, u32), Vec<Loc>>,
    refs: HashMap<(String, u32), Vec<Loc>>,
    asked: Vec<(String, String, u32, u32)>,
}

impl Backend for Fake {
    fn definition(&mut self, path: &str, line: u32, column: u32) -> Result<Vec<Loc>, LspError> {
        self.asked.push(("def".into(), path.into(), line, column));
        Ok(self
            .defs
            .get(&(path.into(), line))
            .cloned()
            .unwrap_or_default())
    }

    fn references(&mut self, path: &str, line: u32, column: u32) -> Result<Vec<Loc>, LspError> {
        self.asked.push(("refs".into(), path.into(), line, column));
        Ok(self
            .refs
            .get(&(path.into(), line))
            .cloned()
            .unwrap_or_default())
    }
}

fn at(path: &str, line: u32, column: u32) -> Loc {
    Loc {
        path: Some(path.into()),
        line,
        column,
    }
}

const OWNER_JAVA: &str = "src/app/Owner.java";
const CONTROLLER_JAVA: &str = "src/app/Controller.java";
const OTHER_JAVA: &str = "src/other/Other.java";
const GET_PET_INT: &str = "src/app/Owner.java#Owner.getPet/1";
const GET_PET_STR: &str = "src/app/Owner.java#Owner.getPet/1~2";

fn round(c: f32) -> f32 {
    (c * 100.0).round() / 100.0
}

/// Call edges to `id`: (caller, confidence rounded, from a server).
fn callers(g: &Graph, id: &str) -> Vec<(String, f32, bool)> {
    let s = g.by_id(id).unwrap();
    let mut v: Vec<_> = g
        .edges_to(s)
        .filter(|e| e.kind == EdgeKind::Calls)
        .map(|e| (g.display_name(e.from), round(e.confidence), e.lsp))
        .collect();
    v.sort_by(|a, b| a.0.cmp(&b.0));
    v
}

fn fake_for_petclinic() -> Fake {
    let mut f = Fake::default();
    // References of getPet(Integer) (declared line 3): Controller.byId,
    // Other.go (unresolved by the heuristic), and a non-call mention.
    f.refs.insert(
        (OWNER_JAVA.into(), 3),
        vec![
            at(CONTROLLER_JAVA, 3, 33),
            at(OTHER_JAVA, 4, 34),
            at(OTHER_JAVA, 2, 11),
        ],
    );
    f.defs
        .insert((CONTROLLER_JAVA.into(), 3), vec![at(OWNER_JAVA, 3, 15)]);
    f.defs
        .insert((CONTROLLER_JAVA.into(), 4), vec![at(OWNER_JAVA, 6, 15)]);
    f.defs
        .insert((OTHER_JAVA.into(), 4), vec![at(OWNER_JAVA, 3, 15)]);
    f
}

#[test]
fn disambiguates_overloads_and_adds_callers() {
    let tmp = tempfile::tempdir().unwrap();
    let files = [
        (OWNER_JAVA, OWNER),
        (CONTROLLER_JAVA, CONTROLLER),
        (OTHER_JAVA, OTHER),
    ];
    let g = graph_on_disk(tmp.path(), &files, &[GET_PET_INT]);
    // The heuristic splits every call between the two overloads.
    let split = [
        ("Controller.byId".into(), 0.43, false),
        ("Controller.byName".into(), 0.43, false),
        ("Other.go".into(), 0.25, false),
    ];
    assert_eq!(callers(&g, GET_PET_INT), split);
    assert_eq!(callers(&g, GET_PET_STR), split);
    // Say it had missed `Other.go`: only the server can find it.
    let mut data = g.data().clone();
    let go = g.find("Other.go")[0];
    data.edges.retain(|e| e.from != go);
    let g = Graph::new(data);

    let mut texts = Texts::new(tmp.path());
    let tasks = refine::plan(&g, &mut texts);
    let kinds: Vec<_> = tasks
        .iter()
        .map(|t| {
            (
                matches!(t.kind, TaskKind::References(_)),
                t.path.as_str(),
                t.line,
            )
        })
        .collect();
    assert_eq!(
        kinds,
        [
            (true, OWNER_JAVA, 3),
            (false, CONTROLLER_JAVA, 3),
            (false, CONTROLLER_JAVA, 4)
        ]
    );
    // `public Pet getPet(Integer id) {`
    assert_eq!(tasks[0].columns, [11]);

    let mut fake = fake_for_petclinic();
    let results: Vec<_> = tasks.iter().map(|t| t.run(&mut fake).ok()).collect();
    let (data, changes) = refine::apply(&g, &tasks, &results, &mut texts);
    let r = Graph::new(data);
    assert_eq!(
        callers(&r, GET_PET_INT),
        [
            ("Controller.byId".into(), 1.0, true),
            ("Other.go".into(), 1.0, true)
        ]
    );
    assert_eq!(
        callers(&r, GET_PET_STR),
        [("Controller.byName".into(), 1.0, true)]
    );
    // The `import app.Owner;` mention is not a call.
    assert_eq!(
        changes,
        Changes {
            confirmed: 2,
            removed: 2,
            added: 1
        }
    );
}

#[test]
fn library_targets_remove_guesses_and_silence_keeps_edges() {
    let tmp = tempfile::tempdir().unwrap();
    let files = [(OWNER_JAVA, OWNER), (CONTROLLER_JAVA, CONTROLLER)];
    let g = graph_on_disk(tmp.path(), &files, &[GET_PET_INT]);
    let mut texts = Texts::new(tmp.path());
    let tasks = refine::plan(&g, &mut texts);
    let mut fake = Fake::default();
    // `o.getPet(1)` resolves into a library; `o.getPet("x")`: no answer.
    fake.defs.insert(
        (CONTROLLER_JAVA.into(), 3),
        vec![Loc {
            path: None,
            line: 1,
            column: 0,
        }],
    );
    let results: Vec<_> = tasks.iter().map(|t| t.run(&mut fake).ok()).collect();
    let (data, changes) = refine::apply(&g, &tasks, &results, &mut texts);
    let r = Graph::new(data);
    assert_eq!(
        callers(&r, GET_PET_INT),
        [("Controller.byName".into(), 0.43, false)]
    );
    assert_eq!(changes.removed, 2);
    assert_eq!(changes.confirmed + changes.added, 0);
}

#[test]
fn refines_with_fake_servers_within_budget() {
    let tmp = tempfile::tempdir().unwrap();
    let files = [
        (OWNER_JAVA, OWNER),
        (CONTROLLER_JAVA, CONTROLLER),
        (OTHER_JAVA, OTHER),
    ];
    let g = graph_on_disk(tmp.path(), &files, &[GET_PET_INT]);
    let cfg = LspConfig::default();
    let opts = Options {
        root: tmp.path(),
        data_dir: None,
        cfg: &cfg,
    };
    let messages = std::sync::Mutex::new(Vec::new());
    let progress = |m: &str| messages.lock().unwrap().push(m.to_string());
    let start = |family: Family, _: Instant, status: &mut dyn FnMut(&str)| -> Started {
        assert_eq!(family, Family::Java);
        status("Importing 50%");
        let report = ServerReport {
            family: Some(family),
            server: "fake".into(),
            status: "ready".into(),
            ..Default::default()
        };
        Ok((Box::new(fake_for_petclinic()), report))
    };
    let (r, stats) = refine_with(&g, &opts, &progress, &start);
    assert_eq!((stats.tasks, stats.answered, stats.failed), (4, 4, 0));
    assert!(stats.complete && stats.any_ready());
    assert_eq!((stats.confirmed, stats.removed, stats.added), (3, 3, 0));
    assert_eq!(r.lsp_stats(), Some(&stats));
    let messages = messages.into_inner().unwrap();
    assert!(messages.contains(&"LSP: refining 0/4 · java indexing… Importing 50%".to_string()));
    assert_eq!(messages.last().unwrap(), "LSP: refining 4/4");
    assert_eq!(
        stats.summary(),
        "LSP: 3 confirmed, 3 removed, 0 added (4/4 answered; fake)"
    );

    // A missing server: the graph is unchanged, the reason reported.
    let missing = |family: Family, _: Instant, _: &mut dyn FnMut(&str)| -> Started {
        Err(ServerReport {
            family: Some(family),
            status: "missing".into(),
            detail: "jdtls not found".into(),
            ..Default::default()
        })
    };
    let (r, stats) = refine_with(&g, &opts, &|_| {}, &missing);
    assert_eq!(r.edges(), g.edges());
    assert!(!stats.any_ready() && !stats.complete);
    assert_eq!(stats.servers[0].status, "missing");
    assert_eq!(stats.servers[0].tasks, 4);

    // No time left: nothing asked.
    let cfg = LspConfig {
        budget_secs: 0,
        ..Default::default()
    };
    let opts = Options { cfg: &cfg, ..opts };
    let (r, stats) = refine_with(&g, &opts, &|_| {}, &start);
    assert_eq!(stats.answered, 0);
    assert_eq!(r.edges(), g.edges());
}

#[test]
fn test_edges_follow_their_calls() {
    let tmp = tempfile::tempdir().unwrap();
    let test = "package app;\n\
public class OwnerTest {\n\
    void t(Owner o) { o.getPet(\"x\"); }\n\
}\n";
    let test_path = "src/test/app/OwnerTest.java";
    let files = [(OWNER_JAVA, OWNER), (test_path, test)];
    let g = graph_on_disk(tmp.path(), &files, &[GET_PET_INT]);
    let tests_to = |g: &Graph, id: &str| -> Vec<(f32, bool)> {
        let s = g.by_id(id).unwrap();
        g.edges_to(s)
            .filter(|e| e.kind == EdgeKind::Tests)
            .map(|e| (round(e.confidence), e.lsp))
            .collect()
    };
    assert_eq!(tests_to(&g, GET_PET_INT), [(0.43, false)]);
    let mut texts = Texts::new(tmp.path());
    let tasks = refine::plan(&g, &mut texts);
    let mut fake = Fake::default();
    fake.defs
        .insert((test_path.into(), 3), vec![at(OWNER_JAVA, 6, 15)]);
    let results: Vec<_> = tasks.iter().map(|t| t.run(&mut fake).ok()).collect();
    let (data, _) = refine::apply(&g, &tasks, &results, &mut texts);
    let r = Graph::new(data);
    assert!(tests_to(&r, GET_PET_INT).is_empty());
    assert_eq!(tests_to(&r, GET_PET_STR), [(1.0, true)]);
}

#[test]
fn plans_calls_of_changed_callables() {
    let tmp = tempfile::tempdir().unwrap();
    let files = [(OWNER_JAVA, OWNER), (CONTROLLER_JAVA, CONTROLLER)];
    let g = graph_on_disk(
        tmp.path(),
        &files,
        &["src/app/Controller.java#Controller.size/1"],
    );
    let mut texts = Texts::new(tmp.path());
    let tasks = refine::plan(&g, &mut texts);
    // `l.size()`: no workspace symbol named `size`, nothing to ask but
    // the references of the changed method.
    assert_eq!(tasks.len(), 1);
    let g = graph_on_disk(
        tmp.path(),
        &files,
        &["src/app/Controller.java#Controller.byId/1"],
    );
    let tasks = refine::plan(&g, &mut texts);
    let defs: Vec<_> = tasks
        .iter()
        .filter_map(|t| match &t.kind {
            TaskKind::Definition { name, from } => {
                Some((name.as_str(), t.line, g.symbol(*from).kind))
            }
            TaskKind::References(_) => None,
        })
        .collect();
    assert_eq!(defs, [("getPet", 3, SymbolKind::Method)]);
}

#[test]
fn resolves_configured_and_builtin_servers() {
    let tmp = tempfile::tempdir().unwrap();
    let exe = tmp.path().join("my-jdtls");
    std::fs::write(&exe, "").unwrap();
    let mut cfg = LspConfig::default();
    cfg.java.command = Some(exe.to_string_lossy().into_owned());
    let spec = resolve(&cfg, Family::Java).unwrap();
    assert_eq!(spec.path, exe);
    assert_eq!(spec.readiness, Readiness::Progress);
    assert!(spec.args.is_empty());
    cfg.java.command = Some(tmp.path().join("nope").to_string_lossy().into_owned());
    assert!(matches!(
        resolve(&cfg, Family::Java),
        Err(Unavailable::Missing(_))
    ));
    cfg.java.enabled = false;
    assert_eq!(resolve(&cfg, Family::Java), Err(Unavailable::Disabled));
    cfg.java.enabled = true;
    cfg.enabled = false;
    assert_eq!(resolve(&cfg, Family::Kotlin), Err(Unavailable::Disabled));
    // The key changes with the servers.
    let a = cache_key("g", &LspConfig::default());
    assert_ne!(a, cache_key("g", &cfg));
    assert_ne!(a, cache_key("h", &LspConfig::default()));

    let spec = ServerSpec {
        family: Family::Java,
        command: "jdtls".into(),
        path: "/bin/jdtls".into(),
        args: vec!["-data".into(), "{data}".into()],
        env: Vec::new(),
        readiness: Readiness::ServiceReady,
    };
    assert_eq!(spec.args_with(Path::new("/d")), ["-data", "/d"]);
    assert_eq!(
        spec.init_options(Path::new("/d"))["settings"]["java"]["import"]["generatesMetadataFilesAtProjectRoot"],
        false
    );
}

#[test]
#[ignore = "needs jdtls in PATH"]
fn real_jdtls_disambiguates_overloads() {
    let tmp = tempfile::tempdir().unwrap();
    let pet = "package app;\npublic class Pet {}\n";
    let files = [
        (OWNER_JAVA, OWNER),
        (CONTROLLER_JAVA, CONTROLLER),
        ("src/app/Pet.java", pet),
    ];
    let g = graph_on_disk(tmp.path(), &files, &[GET_PET_INT]);
    let cfg = LspConfig {
        budget_secs: 120,
        ..Default::default()
    };
    let data = tmp.path().join(".lsp");
    let opts = Options {
        root: tmp.path(),
        data_dir: Some(&data),
        cfg: &cfg,
    };
    let (r, stats) = refine(&g, &opts, &|m| eprintln!("{m}"));
    assert!(stats.any_ready(), "{stats:?}");
    assert_eq!(
        callers(&r, GET_PET_INT),
        [("Controller.byId".into(), 1.0, true)]
    );
}
