use super::*;
use crate::index::{Index, parse_file};

/// Index of inline head files, and base files.
fn index(head: &[(&str, &str)], base: &[(&str, &str)]) -> Index {
    let parse = |files: &[(&str, &str)]| {
        let mut v: Vec<_> = files
            .iter()
            .map(|(p, s)| parse_file(p, Lang::from_path(p).unwrap(), s))
            .collect();
        v.sort_by(|a, b| a.path.cmp(&b.path));
        v
    };
    Index {
        files: parse(head),
        base_files: parse(base),
        stats: Default::default(),
    }
}

fn graph(head: &[(&str, &str)]) -> Graph {
    build(&index(head, &[]), &Default::default(), &default_rules())
}

/// The symbol named `qualified` (constructors aside).
fn sym(g: &Graph, qualified: &str) -> SymIdx {
    let mut found = g.find(qualified);
    if found.len() > 1 {
        found.retain(|&s| g.symbol(s).kind != SymbolKind::Constructor);
    }
    assert_eq!(found.len(), 1, "{qualified}: {found:?}");
    found[0]
}

/// (display name, rounded confidence) of links.
fn names(g: &Graph, links: &[Link]) -> Vec<(String, f32)> {
    let mut v: Vec<_> = links
        .iter()
        .map(|l| {
            (
                g.display_name(l.symbol),
                (l.confidence * 100.0).round() / 100.0,
            )
        })
        .collect();
    v.sort_by(|a, b| a.0.cmp(&b.0));
    v
}

const REPO: &str = "package app.owner;\n\
public interface OwnerRepository {\n\
    Owner findById(int id);\n\
    void save(Owner owner);\n\
}\n";

const SERVICE: &str = "package app.owner;\n\
import app.util.Strings;\n\
import org.lib.External;\n\
public class OwnerService {\n\
    private final OwnerRepository repo;\n\
    OwnerService(OwnerRepository repo) { this.repo = repo; }\n\
    public Owner find(int id) {\n\
        Strings.check(id);\n\
        return repo.findById(id);\n\
    }\n\
    public void rename(Owner o, String name) {\n\
        o.setName(name);\n\
        repo.save(o);\n\
        External.save(o);\n\
    }\n\
}\n";

const CONTROLLER: &str = "package app.web;\n\
import app.owner.OwnerService;\n\
public class OwnerController {\n\
    private final OwnerService service;\n\
    OwnerController(OwnerService service) { this.service = service; }\n\
    Object show(int id) { return service.find(id); }\n\
    Object other(int id) { return lookup().rename(null, \"x\"); }\n\
}\n";

const OWNER: &str = "package app.owner;\n\
public class Owner {\n\
    private String name;\n\
    public void setName(String name) { this.name = name; }\n\
}\n";

const STRINGS: &str = "package app.util;\n\
public class Strings {\n\
    public static void check(int a) {}\n\
    public static void check(String a) {}\n\
    public static void check(String a, String b) {}\n\
}\n";

const TEST: &str = "package app.owner;\n\
class OwnerServiceTest {\n\
    OwnerService service;\n\
    void findsOwner() { service.find(1); }\n\
}\n";

fn java_graph() -> Graph {
    graph(&[
        ("src/main/java/app/owner/OwnerRepository.java", REPO),
        ("src/main/java/app/owner/OwnerService.java", SERVICE),
        ("src/main/java/app/owner/Owner.java", OWNER),
        ("src/main/java/app/util/Strings.java", STRINGS),
        ("src/main/java/app/web/OwnerController.java", CONTROLLER),
        ("src/test/java/app/owner/OwnerServiceTest.java", TEST),
    ])
}

#[test]
fn resolves_typed_receivers_imports_and_arity() {
    let g = java_graph();
    let find = sym(&g, "OwnerService.find");
    // Typed field, imported type: certain.
    assert_eq!(
        names(&g, &g.callers(find)),
        [
            ("OwnerController.show".to_string(), 0.9),
            ("OwnerServiceTest.findsOwner".to_string(), 0.85)
        ]
    );
    // Unknown receiver (`lookup()`): the only `rename` of a visible type.
    let rename = sym(&g, "OwnerService.rename");
    assert_eq!(
        names(&g, &g.callers(rename)),
        [("OwnerController.other".to_string(), 0.5)]
    );
    // Same-package interface, typed field; overloads narrowed by arity:
    // `check(id)` has two one-parameter candidates, never the two-parameter one.
    let callees = names(&g, &g.callees(find));
    assert_eq!(
        callees,
        [
            ("OwnerRepository.findById".to_string(), 0.85),
            ("Strings.check".to_string(), 0.45),
            ("Strings.check".to_string(), 0.45)
        ]
    );
    // A library receiver (`External`, imported from outside) gives no edge,
    // although the project has a `save` method.
    let rename = sym(&g, "OwnerService.rename");
    let callees = names(&g, &g.callees(rename));
    assert_eq!(
        callees,
        [
            ("Owner.setName".to_string(), 0.85),
            ("OwnerRepository.save".to_string(), 0.85)
        ]
    );
}

#[test]
fn test_edges_from_calls_and_naming() {
    let g = java_graph();
    let find = sym(&g, "OwnerService.find");
    let tests = g.tests_of(find);
    assert_eq!(g.symbol(tests[0].symbol).name, "findsOwner");
    assert_eq!(tests[0].via, None);
    // The naming convention links the test class to the class; the method
    // sees it through its class.
    let class = sym(&g, "OwnerService");
    let by_naming = g.tests_of(class);
    assert_eq!(
        names(&g, &by_naming),
        [("OwnerServiceTest".to_string(), 0.72)]
    );
    let rename = sym(&g, "OwnerService.rename");
    let via: Vec<_> = g.tests_of(rename).iter().map(|l| l.via).collect();
    assert_eq!(via, [Some(class)]);
    // Test code is never a candidate for main code.
    assert!(
        g.symbol(sym(&g, "OwnerServiceTest.findsOwner"))
            .has_role(Role::Test)
    );
    assert!(!g.symbol(find).has_role(Role::Test));
}

#[test]
fn ambiguous_names_split_confidence() {
    let a = "package p;\nclass A { void reindex(int x) {} }\n";
    let b = "package p;\nclass B { void reindex(int x) {} }\n";
    let c = "package p;\nclass C { void run() { make().reindex(1); unknown.reindex(2); } }\n";
    let g = graph(&[("A.java", a), ("B.java", b), ("C.java", c)]);
    let run = sym(&g, "C.run");
    // Two same-package candidates for an unknown receiver: both, halved.
    assert_eq!(
        names(&g, &g.callees(run)),
        [
            ("A.reindex".to_string(), 0.25),
            ("B.reindex".to_string(), 0.25)
        ]
    );
    // Too common a name on an unknown receiver: no guess at all.
    let d = "package p;\nclass D { void get() {} void run() { x().get(); } }\n";
    let g = graph(&[
        ("D.java", d),
        (
            "E.java",
            "package p;\nclass E { void run() { y().get(); } }\n",
        ),
    ]);
    assert!(g.callees(sym(&g, "E.run")).is_empty());
}

#[test]
fn inheritance_and_overrides() {
    let base = "package p;\npublic interface Store { void put(int k); }\n";
    let imp =
        "package p;\npublic class MemStore implements Store {\n public void put(int k) {}\n}\n";
    let user = "package p;\nclass User { Store store; void go() { store.put(1); } }\n";
    let g = graph(&[
        ("Store.java", base),
        ("MemStore.java", imp),
        ("User.java", user),
    ]);
    let mem = sym(&g, "MemStore");
    let inherits: Vec<_> = g
        .edges_from(mem)
        .filter(|e| e.kind == EdgeKind::Inherits)
        .collect();
    assert_eq!(inherits.len(), 1);
    assert_eq!(g.symbol(inherits[0].to).name, "Store");
    // Callers of the implementation through the interface (dynamic dispatch).
    let put = sym(&g, "MemStore.put");
    let callers = g.callers(put);
    assert_eq!(names(&g, &callers)[0].0, "User.go");
    assert_eq!(callers[0].via, Some(sym(&g, "Store.put")));
}

#[test]
fn kotlin_and_typescript_resolution() {
    let repo = "package app\n\ninterface Repo {\n    fun load(id: Int): String\n}\n";
    let svc = "package app\n\nclass Svc(private val repo: Repo) {\n    fun get(id: Int) = repo.load(id)\n\n    companion object {\n        fun create(r: Repo) = Svc(r)\n    }\n}\n\nfun main() {\n    Svc.create(TODO()).get(1)\n}\n";
    let g = graph(&[("src/Repo.kt", repo), ("src/Svc.kt", svc)]);
    assert_eq!(
        names(&g, &g.callees(sym(&g, "Svc.get"))),
        [("Repo.load".to_string(), 0.85)]
    );
    // Kotlin constructor call, companion member as a static call.
    assert_eq!(
        names(&g, &g.callees(sym(&g, "Companion.create"))),
        [("Svc".to_string(), 1.0)]
    );
    assert_eq!(
        names(&g, &g.callees(sym(&g, "main"))),
        [("Companion.create".to_string(), 0.95)]
    );

    let service = "import { HttpClient } from '@angular/common/http';\n\
export class OwnerService {\n  constructor(private http: HttpClient) {}\n  getOwners() { return this.http.get('/owners'); }\n}\n";
    let comp = "import { OwnerService } from '../owner.service';\n\
export class ListComponent {\n  constructor(private owners: OwnerService) {}\n  ngOnInit() { this.owners.getOwners(); }\n}\n";
    let g = graph(&[
        ("src/app/owner.service.ts", service),
        ("src/app/list/list.component.ts", comp),
    ]);
    let get = sym(&g, "OwnerService.getOwners");
    assert_eq!(
        names(&g, &g.callers(get)),
        [("ListComponent.ngOnInit".to_string(), 0.9)]
    );
    // `http.get` is a library call: no edge.
    assert!(g.callees(get).is_empty());
    let imports: Vec<_> = g
        .edges()
        .iter()
        .filter(|e| e.kind == EdgeKind::Imports)
        .map(|e| g.symbol(e.to).name.clone())
        .collect();
    assert_eq!(imports, ["OwnerService"]);
}

#[test]
fn maps_hunks_to_symbols() {
    let base_owner = "package app.owner;\n\
public class Owner {\n\
    private String name;\n\
    public void setName(String name) { this.name = name; }\n\
    public void legacy() {}\n\
}\n";
    let raw = concat!(
        "diff --git a/Owner.java b/Owner.java\n--- a/Owner.java\n+++ b/Owner.java\n",
        "@@ -4,3 +4,2 @@\n",
        "-    public void setName(String name) { this.name = name; }\n",
        "-    public void legacy() {}\n",
        "+    public void setName(String name) { this.name = name.trim(); }\n",
        " }\n",
    );
    let diff = crate::diff::parse(raw.as_bytes()).unwrap();
    let idx = index(&[("Owner.java", OWNER)], &[("Owner.java", base_owner)]);
    let g = build(&idx, &diff, &default_rules());
    let syms: Vec<_> = g
        .symbols_of_hunk(0)
        .iter()
        .map(|&s| (g.display_name(s), g.symbol(s).removed))
        .collect();
    assert_eq!(
        syms,
        [
            ("Owner.setName".to_string(), false),
            ("Owner.legacy".to_string(), true)
        ]
    );
    let set_name = sym(&g, "Owner.setName");
    assert!(g.symbol(set_name).changed);
    assert_eq!(g.hunks_of_symbol(set_name), [0]);
    assert!(!g.symbol(sym(&g, "Owner")).changed);
    assert_eq!(
        g.symbol(sym(&g, "Owner.legacy")).container,
        Some(sym(&g, "Owner"))
    );
    let changed: Vec<_> = g
        .changed_symbols()
        .iter()
        .map(|&s| g.display_name(s))
        .collect();
    assert_eq!(changed, ["Owner.setName", "Owner.legacy"]);
    assert!(g.is_file_changed("Owner.java"));

    assert_eq!(g.symbol_at("Owner.java", 4), Some(set_name));
    assert_eq!(
        g.symbol_at("Owner.java", 3).map(|s| g.symbol(s).kind),
        Some(SymbolKind::Field)
    );
    assert_eq!(
        g.symbol_at("Owner.java", 1).map(|s| g.symbol(s).kind),
        Some(SymbolKind::File)
    );
}

#[test]
fn module_map_counts_dependencies() {
    let g = java_graph();
    let map = g.module_map();
    let names: Vec<_> = map.modules.iter().map(|m| m.name.as_str()).collect();
    assert_eq!(names, ["app.owner", "app.util", "app.web"]);
    let edge = |from: &str, to: &str| {
        map.edges
            .iter()
            .find(|e| map.modules[e.from].name == from && map.modules[e.to].name == to)
    };
    let web_owner = edge("app.web", "app.owner").unwrap();
    assert!(web_owner.kinds.contains_key(&EdgeKind::Calls));
    assert!(web_owner.kinds.contains_key(&EdgeKind::Imports));
    assert!(edge("app.owner", "app.util").is_some());
    assert!(edge("app.owner", "app.web").is_none());
    assert!(!map.modules[0].changed());
}

#[test]
fn rules_extend_the_graph_and_the_cache_round_trips() {
    struct Mark;
    impl FrameworkRule for Mark {
        fn name(&self) -> &str {
            "mark"
        }
        fn apply(&self, index: &Index, graph: &mut GraphBuilder) {
            for (fi, f) in index.files.iter().enumerate() {
                for (d, def) in f.defs.iter().enumerate() {
                    if def.name == "OwnerController" {
                        let s = graph.def_symbol(fi, d);
                        graph.add_role(s, Role::EntryPoint);
                        graph
                            .symbol_mut(s)
                            .tags
                            .insert("http.route".into(), "/owners".into());
                        let target = graph.resolve_type(fi, "OwnerService")[0].0;
                        graph.add_edge(s, target, EdgeKind::Injects, 0.9, def.line);
                    }
                }
            }
        }
    }
    let idx = index(
        &[
            ("src/app/owner/OwnerService.java", SERVICE),
            ("src/app/web/OwnerController.java", CONTROLLER),
        ],
        &[],
    );
    let rules: Vec<Box<dyn FrameworkRule>> = vec![Box::new(Mark)];
    let mut g = build(&idx, &Default::default(), &rules);
    let ctl = sym(&g, "OwnerController");
    assert_eq!(g.symbol(ctl).roles, [Role::EntryPoint]);
    assert!(g.edges_from(ctl).any(|e| e.kind == EdgeKind::Injects));
    assert_eq!(g.stats().rules, ["mark"]);

    let tmp = tempfile::tempdir().unwrap();
    let path = cache_path(tmp.path(), "abc");
    let key = cache_key("base", "abc", &rules, &[]);
    g.set_origin(key.clone(), "base".into(), "abc".into());
    save_cache(&path, &g).unwrap();
    let loaded = load_cache(&path, &key).unwrap();
    assert!(loaded.from_cache);
    assert_eq!(loaded.symbols(), g.symbols());
    assert_eq!(loaded.edges(), g.edges());
    assert_eq!(
        loaded.callers(sym(&g, "OwnerService.find")),
        g.callers(sym(&g, "OwnerService.find"))
    );
    assert!(load_cache(&path, &cache_key("base", "abc", &default_rules(), &[])).is_none());
}

#[test]
fn test_paths() {
    for p in [
        "src/test/java/a/FooTest.java",
        "a/FooTests.kt",
        "a/FooIT.java",
        "src/app/x.spec.ts",
        "lib/__tests__/x.js",
        "web/x.test.tsx",
    ] {
        assert!(is_test_path(p), "{p}");
    }
    for p in [
        "src/main/java/a/Test.java",
        "src/main/java/a/Testing.java",
        "src/app/latest.ts",
    ] {
        assert!(!is_test_path(p), "{p}");
    }
}
