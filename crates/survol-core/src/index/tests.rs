use super::*;
use crate::git::testutil::{commit, repo};

fn parse(path: &str, src: &str) -> FileIndex {
    parse_file(path, Lang::from_path(path).unwrap(), src)
}

fn def<'a>(f: &'a FileIndex, name: &str) -> &'a Def {
    f.defs
        .iter()
        .find(|d| d.name == name)
        .unwrap_or_else(|| panic!("no def {name} in {:?}", f.defs))
}

fn parent_name<'a>(f: &'a FileIndex, d: &Def) -> Option<&'a str> {
    d.parent.map(|p| f.defs[p as usize].name.as_str())
}

fn calls(f: &FileIndex) -> Vec<(String, Option<String>, Option<u16>)> {
    f.refs
        .iter()
        .filter(|r| r.kind == RefKind::Call)
        .map(|r| (r.name.clone(), r.receiver.clone(), r.arity))
        .collect()
}

fn refs_of(f: &FileIndex, kind: RefKind) -> Vec<&str> {
    f.refs
        .iter()
        .filter(|r| r.kind == kind)
        .map(|r| r.name.as_str())
        .collect()
}

fn binding<'a>(f: &'a FileIndex, name: &str) -> &'a Binding {
    f.bindings
        .iter()
        .find(|b| b.name == name)
        .unwrap_or_else(|| panic!("no binding {name} in {:?}", f.bindings))
}

const JAVA: &str = r#"package org.acme.owner;

import static org.acme.util.Checks.notNull;
import org.acme.model.*;
import org.springframework.web.bind.annotation.GetMapping;

@RestController
@RequestMapping(value = "/owners", produces = {"a", "b"})
public class OwnerController extends BaseController implements Api, Other {
    private final OwnerRepository owners;

    OwnerController(OwnerRepository owners) {
        this.owners = owners;
    }

    @GetMapping("/{id}")
    public Owner find(@PathVariable("id") int id, String... tags) {
        notNull(id);
        for (Pet pet : owners.findById(id, 2).getPets()) {
            pet.check();
        }
        return new Owner(id);
    }

    static class Inner {
        void run() {}
    }
}
"#;

#[test]
fn java_definitions_and_annotations() {
    let f = parse("src/OwnerController.java", JAVA);
    assert!(!f.has_errors);
    assert_eq!(f.package.as_deref(), Some("org.acme.owner"));
    let class = def(&f, "OwnerController");
    assert_eq!(class.kind, SymbolKind::Class);
    assert_eq!(class.span.start, 7, "span starts at the first annotation");
    assert_eq!(class.line, 9);
    let names: Vec<_> = class.annotations.iter().map(|a| a.name.as_str()).collect();
    assert_eq!(names, ["RestController", "RequestMapping"]);
    let mapping = &class.annotations[1];
    assert_eq!(mapping.value().unwrap().as_str(), Some("/owners"));
    assert_eq!(mapping.arg("produces").unwrap().strings(), ["a", "b"]);

    let find = def(&f, "find");
    assert_eq!(find.kind, SymbolKind::Method);
    assert_eq!(parent_name(&f, find), Some("OwnerController"));
    assert_eq!((find.params, find.variadic), (Some(2), true));
    assert_eq!(find.annotations[0].name, "GetMapping");
    assert_eq!(find.annotations[0].value().unwrap().as_str(), Some("/{id}"));

    let ctor = f
        .defs
        .iter()
        .find(|d| d.kind == SymbolKind::Constructor)
        .unwrap();
    assert_eq!(ctor.params, Some(1));
    assert_eq!(def(&f, "owners").kind, SymbolKind::Field);
    let run = def(&f, "run");
    assert_eq!(parent_name(&f, run), Some("Inner"));
    assert_eq!(
        f.qualified_name(f.defs.iter().position(|d| d.name == "run").unwrap()),
        "OwnerController.Inner.run"
    );
}

#[test]
fn java_references_imports_and_bindings() {
    let f = parse("src/OwnerController.java", JAVA);
    let c = calls(&f);
    assert!(c.contains(&("notNull".into(), None, Some(1))));
    assert!(c.contains(&("findById".into(), Some("owners".into()), Some(2))));
    assert!(c.contains(&("check".into(), Some("pet".into()), Some(0))));
    assert_eq!(refs_of(&f, RefKind::New), ["Owner"]);
    assert_eq!(refs_of(&f, RefKind::Extends), ["BaseController"]);
    assert_eq!(refs_of(&f, RefKind::Implements), ["Api", "Other"]);
    assert!(refs_of(&f, RefKind::Type).contains(&"OwnerRepository"));
    let scope = f.refs.iter().find(|r| r.name == "findById").unwrap().scope;
    assert_eq!(
        scope.map(|s| f.defs[s as usize].name.as_str()),
        Some("find")
    );

    assert_eq!(f.imports.len(), 3);
    assert!(f.imports[0].is_static);
    assert_eq!(f.imports[0].module, "org.acme.util.Checks");
    assert_eq!(f.imports[0].names[0].name, "notNull");
    assert!(f.imports[1].wildcard);
    assert_eq!(f.imports[1].module, "org.acme.model");
    assert_eq!(
        f.imports[2].module,
        "org.springframework.web.bind.annotation"
    );

    let owners = binding(&f, "owners");
    assert!(owners.field);
    assert_eq!(owners.type_name, "OwnerRepository");
    assert_eq!(
        owners.scope.map(|s| f.defs[s as usize].name.as_str()),
        Some("OwnerController")
    );
    assert_eq!(binding(&f, "pet").type_name, "Pet");
    assert_eq!(binding(&f, "tags").type_name, "String");
}

const KOTLIN: &str = r#"package org.acme.owner

import org.acme.model.Owner as Person
import org.acme.util.*

@Service
class OwnerService(private val repo: OwnerRepository) : Base(), Api {
    constructor(x: Int) : this(Repo())

    @Transactional(readOnly = true)
    fun find(id: Int, vararg tags: String): Person? {
        val cache = Cache(3)
        return repo.findById(id).also { cache.put(it) }
    }

    companion object {
        fun make() = OwnerService(Repo())
    }
}

interface Api {
    fun find(id: Int): Person?
}

enum class Color { RED, GREEN }

object Registry {
    fun register() = helper()
}
"#;

#[test]
fn kotlin_definitions_and_references() {
    let f = parse("src/OwnerService.kt", KOTLIN);
    assert!(!f.has_errors, "{f:?}");
    assert_eq!(f.package.as_deref(), Some("org.acme.owner"));
    let class = def(&f, "OwnerService");
    assert_eq!((class.kind, class.params), (SymbolKind::Class, Some(1)));
    assert_eq!(class.annotations[0].name, "Service");
    let ctor = f
        .defs
        .iter()
        .find(|d| d.kind == SymbolKind::Constructor)
        .unwrap();
    assert_eq!((ctor.name.as_str(), ctor.params), ("OwnerService", Some(1)));
    let find = f
        .defs
        .iter()
        .find(|d| {
            d.name == "find"
                && d.parent
                    .is_some_and(|p| f.defs[p as usize].name == "OwnerService")
        })
        .unwrap();
    assert_eq!(
        (find.kind, find.params, find.variadic),
        (SymbolKind::Method, Some(2), true)
    );
    let tx = &find.annotations[0];
    assert_eq!(tx.name, "Transactional");
    assert_eq!(tx.arg("readOnly").unwrap().value, "true");
    let companion = def(&f, "Companion");
    assert_eq!(companion.kind, SymbolKind::Object);
    assert_eq!(parent_name(&f, def(&f, "make")), Some("Companion"));
    assert_eq!(def(&f, "Api").kind, SymbolKind::Interface);
    assert_eq!(def(&f, "Color").kind, SymbolKind::Enum);
    assert_eq!(def(&f, "Registry").kind, SymbolKind::Object);
    assert_eq!(def(&f, "RED").kind, SymbolKind::Field);

    let c = calls(&f);
    assert!(c.contains(&("findById".into(), Some("repo".into()), Some(1))));
    assert!(c.contains(&("also".into(), Some("repo.findById(id)".into()), Some(1))));
    assert!(c.contains(&("OwnerService".into(), None, Some(1))));
    assert!(c.contains(&("helper".into(), None, Some(0))));
    assert_eq!(refs_of(&f, RefKind::Extends), ["Base"]);
    assert_eq!(refs_of(&f, RefKind::Implements), ["Api"]);

    let repo = binding(&f, "repo");
    assert!(repo.field);
    assert_eq!(repo.type_name, "OwnerRepository");
    assert_eq!(
        binding(&f, "cache").type_name,
        "Cache",
        "inferred from a constructor call"
    );

    assert_eq!(f.imports[0].module, "org.acme.model");
    assert_eq!(f.imports[0].names[0].name, "Owner");
    assert_eq!(f.imports[0].names[0].local(), "Person");
    assert!(f.imports[1].wildcard);
    assert_eq!(f.imports[1].module, "org.acme.util");
}

const TS: &str = r#"import { Injectable, inject } from '@angular/core';
import Default, * as ns from './x';
import { OwnerService as Owners } from '../owner.service';
export { Pet } from './pet';
const legacy = require('./legacy');

@Component({
  selector: 'app-owner-list',
  templateUrl: './owner-list.component.html',
})
export class OwnerListComponent extends Base implements OnInit {
  private readonly cdr = inject(ChangeDetectorRef);
  @Input() owner: Owner;

  constructor(private router: Router, private owners: Owners) {}

  @HostListener('click', ['$event'])
  onClick(e: Event, ...rest) {
    this.owners.getOwners(1).subscribe(o => this.render(o));
  }

  private render = (o: Owner) => ns.helper(o);
}

export function top(a, b = 2) { return new OwnerListComponent(a); }
export const arrow = x => x;
"#;

#[test]
fn typescript_decorators_imports_and_bindings() {
    let f = parse("src/app/owner-list.component.ts", TS);
    assert!(!f.has_errors);
    let class = def(&f, "OwnerListComponent");
    assert_eq!(class.span.start, 7, "decorator on the export statement");
    let comp = &class.annotations[0];
    assert_eq!(comp.name, "Component");
    assert_eq!(
        comp.arg("selector").unwrap().as_str(),
        Some("app-owner-list")
    );
    assert_eq!(
        comp.arg("templateUrl").unwrap().as_str(),
        Some("./owner-list.component.html")
    );
    let owner = def(&f, "owner");
    assert_eq!(
        (owner.kind, owner.annotations[0].name.as_str()),
        (SymbolKind::Field, "Input")
    );
    let click = def(&f, "onClick");
    assert_eq!(click.annotations[0].name, "HostListener");
    assert_eq!(click.annotations[0].args.len(), 2);
    assert_eq!((click.params, click.variadic), (Some(2), true));
    assert_eq!(def(&f, "constructor").kind, SymbolKind::Constructor);
    assert_eq!(def(&f, "render").kind, SymbolKind::Method);
    assert_eq!(
        (def(&f, "top").kind, def(&f, "top").params),
        (SymbolKind::Function, Some(2))
    );
    assert_eq!(def(&f, "arrow").params, Some(1));

    let c = calls(&f);
    assert!(c.contains(&("getOwners".into(), Some("this.owners".into()), Some(1))));
    assert!(c.contains(&("render".into(), Some("this".into()), Some(1))));
    assert!(c.contains(&("helper".into(), Some("ns".into()), Some(1))));
    assert!(
        !c.iter().any(|(n, _, _)| n == "Component" || n == "Input"),
        "decorators are not calls"
    );
    assert_eq!(refs_of(&f, RefKind::New), ["OwnerListComponent"]);
    assert_eq!(refs_of(&f, RefKind::Extends), ["Base"]);
    assert_eq!(refs_of(&f, RefKind::Implements), ["OnInit"]);

    for (name, ty) in [
        ("router", "Router"),
        ("owners", "Owners"),
        ("cdr", "ChangeDetectorRef"),
        ("owner", "Owner"),
    ] {
        let b = binding(&f, name);
        assert!(b.field, "{name}");
        assert_eq!(b.type_name, ty);
        assert_eq!(
            b.scope.map(|s| f.defs[s as usize].name.as_str()),
            Some("OwnerListComponent")
        );
    }

    let m: Vec<_> = f.imports.iter().map(|i| i.module.as_str()).collect();
    assert_eq!(
        m,
        [
            "@angular/core",
            "./x",
            "../owner.service",
            "./pet",
            "./legacy"
        ]
    );
    let x = &f.imports[1];
    assert_eq!(
        (x.names[0].name.as_str(), x.names[0].local()),
        ("default", "Default")
    );
    assert!(x.wildcard);
    assert_eq!(x.alias.as_deref(), Some("ns"));
    assert_eq!(f.imports[2].names[0].local(), "Owners");
    assert_eq!(f.imports[3].names[0].name, "Pet");
    assert!(f.imports[4].wildcard);
}

#[test]
fn javascript_and_tsx() {
    let f = parse(
        "web/app.js",
        "const Api = require('./api');\nclass Store extends Base {\n  load(id) { const api = new Api(); return api.fetch(id); }\n}\nfunction main() { new Store().load(1); }\n",
    );
    assert!(!f.has_errors);
    assert_eq!(def(&f, "load").kind, SymbolKind::Method);
    assert_eq!(def(&f, "main").kind, SymbolKind::Function);
    assert_eq!(binding(&f, "api").type_name, "Api");
    assert!(calls(&f).contains(&("fetch".into(), Some("api".into()), Some(1))));
    assert_eq!(refs_of(&f, RefKind::Extends), ["Base"]);
    assert_eq!(f.imports[0].module, "./api");

    let f = parse(
        "web/View.tsx",
        "export function View({ id }: Props) { return <div onClick={() => save(id)}>{id}</div>; }\n",
    );
    assert!(!f.has_errors);
    assert_eq!(def(&f, "View").params, Some(1));
    assert!(calls(&f).contains(&("save".into(), None, Some(1))));
}

#[test]
fn syntax_errors_give_partial_results() {
    let f = parse("A.java", "class A {\n  void ok() {}\n  void broken( {\n}\n");
    assert!(f.has_errors);
    assert!(f.defs.iter().any(|d| d.name == "A"));
}

#[test]
fn ids_tell_overloads_apart() {
    let f = parse(
        "src/A.java",
        "class A {\n void f(int a) {}\n void f(String a) {}\n void f() {}\n}\n",
    );
    assert_eq!(
        f.def_ids(),
        [
            "src/A.java#A",
            "src/A.java#A.f/1",
            "src/A.java#A.f/1~2",
            "src/A.java#A.f/0"
        ]
    );
    assert_eq!(f.def_at(3).map(|d| f.defs[d].params), Some(Some(1)));
    assert_eq!(f.def_at(1).map(|d| f.defs[d].name.as_str()), Some("A"));
}

#[test]
fn builds_from_git_with_blob_cache() {
    let tmp = tempfile::tempdir().unwrap();
    let g = repo(tmp.path());
    let base = commit(
        &g,
        &[
            ("src/A.java", "class A { void old() {} }\n"),
            ("src/B.kt", "class B\n"),
            ("node_modules/x/index.js", "function x() {}\n"),
            ("README.md", "# hi\n"),
        ],
        "base",
    );
    let head = commit(
        &g,
        &[
            ("src/A.java", "class A { void renamed() {} }\n"),
            ("web/c.ts", "export const c = () => 1;\n"),
        ],
        "head",
    );
    let survol = g.survol_dir().unwrap();
    let opts = Options::new(&[], Some(&survol)).unwrap();
    let base_paths = vec!["src/A.java".to_string()];
    let build = || Index::build(&g, &head, Some((&base, &base_paths)), &opts, &mut |_| {}).unwrap();

    let first = build();
    let paths: Vec<_> = first.files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(paths, ["src/A.java", "src/B.kt", "web/c.ts"]);
    assert_eq!(first.stats.skipped, 1, "node_modules");
    assert_eq!(first.stats.parsed, 4);
    assert!(
        first
            .file("src/A.java")
            .unwrap()
            .defs
            .iter()
            .any(|d| d.name == "renamed")
    );
    assert!(
        first
            .base_file("src/A.java")
            .unwrap()
            .defs
            .iter()
            .any(|d| d.name == "old")
    );

    let second = build();
    assert_eq!(second.stats.parsed, 0, "everything comes from the cache");
    assert_eq!(second.files, first.files);
    assert_eq!(second.base_files, first.base_files);
}

#[test]
fn framework_facts_java_kotlin() {
    let f = parse(
        "src/A.java",
        r#"package app;
@Service
public abstract class A {
    private static final String BASE = "/api" + "/v1";
    private final Repo repo;
    A(@Qualifier("main") Repo repo, @Value("${a.b}") int n) { this.repo = repo; }
    @Bean public Clock clock() { publisher.publishEvent(new Ev(1)); return null; }
}
"#,
    );
    let a = def(&f, "A");
    assert_eq!(a.modifiers, ["abstract"]);
    let repo = def(&f, "repo");
    assert_eq!(repo.type_name.as_deref(), Some("Repo"));
    assert_eq!(repo.modifiers, ["final"]);
    assert_eq!(def(&f, "clock").type_name.as_deref(), Some("Clock"));
    let base = f.values.iter().find(|v| v.name == "BASE").unwrap();
    assert_eq!(base.text, "\"/api\" + \"/v1\"");
    assert_eq!(base.scope, Some(0));
    let p = f
        .bindings
        .iter()
        .find(|b| b.name == "repo" && !b.field)
        .unwrap();
    assert_eq!(p.annotations[0].name, "Qualifier");
    // Primitive parameter kept because annotated.
    let n = binding(&f, "n");
    assert_eq!(n.annotations[0].name, "Value");
    let publish = f.refs.iter().find(|r| r.name == "publishEvent").unwrap();
    assert_eq!(publish.arg.as_deref(), Some("new Ev(1)"));

    let k = parse(
        "src/B.kt",
        r#"package app
@Service
class B(@Qualifier("q") private val repo: Repo) {
    @Autowired lateinit var clock: Clock
    private val url = "http://x/" + path
    fun run(@Value("\${a}") a: String): Result { events.publishEvent(Ev(a)) }
}
"#,
    );
    let clock = def(&k, "clock");
    assert_eq!(clock.type_name.as_deref(), Some("Clock"));
    assert!(clock.has_modifier("lateinit"));
    assert_eq!(def(&k, "run").type_name.as_deref(), Some("Result"));
    assert_eq!(binding(&k, "repo").annotations[0].name, "Qualifier");
    assert_eq!(binding(&k, "a").annotations[0].name, "Value");
    assert!(k.values.iter().any(|v| v.name == "url"));
    let publish = k.refs.iter().find(|r| r.name == "publishEvent").unwrap();
    assert_eq!(publish.arg.as_deref(), Some("Ev(a)"));
}

#[test]
fn framework_facts_typescript() {
    let f = parse(
        "src/app/app.routes.ts",
        r#"export const API = new InjectionToken<string>('api');
const base = environment.api + 'owners';
export const routes: Routes = [
  { path: 'owners', component: OwnerListComponent, children: [
    { path: ':id', loadComponent: () => import('./owner.component').then(m => m.OwnerComponent) },
  ]},
  { path: 'vets', loadChildren: './vets/vets.module#VetsModule' },
  { path: 'x', method: 'GET' },
];
export class S {
  private readonly url = `${base}/x`;
  constructor(@Inject(API) private api: string) {}
  get(id: number): Observable<X> { return this.http.get<X>(`${this.url}/${id}`); }
}
"#,
    );
    // Top-level constants are definitions.
    assert_eq!(def(&f, "routes").kind, SymbolKind::Field);
    assert_eq!(def(&f, "API").kind, SymbolKind::Field);
    assert!(def(&f, "url").has_modifier("readonly"));
    assert_eq!(def(&f, "get").type_name.as_deref(), Some("Observable"));
    let names: Vec<&str> = f.values.iter().map(|v| v.name.as_str()).collect();
    assert_eq!(names, ["base", "url"]);
    let routes: Vec<(&str, Option<&str>, Option<u32>)> = f
        .routes
        .iter()
        .map(|r| (r.path.as_str(), r.component.as_deref(), r.parent))
        .collect();
    assert_eq!(
        routes,
        [
            ("owners", Some("OwnerListComponent"), None),
            (":id", None, Some(0)),
            ("vets", None, None)
        ]
    );
    let lazy = f.routes[1].load.as_ref().unwrap();
    assert_eq!(
        (lazy.children, lazy.module.as_str()),
        (false, "./owner.component")
    );
    assert_eq!(lazy.export.as_deref(), Some("OwnerComponent"));
    let old = f.routes[2].load.as_ref().unwrap();
    assert_eq!(
        (old.children, old.module.as_str()),
        (true, "./vets/vets.module")
    );
    assert_eq!(old.export.as_deref(), Some("VetsModule"));
    assert_eq!(f.routes[0].scope, Some(2), "routes belong to the constant");
    assert_eq!(binding(&f, "api").annotations[0].name, "Inject");
    let get = f.refs.iter().find(|r| r.name == "get").unwrap();
    assert_eq!(get.arg.as_deref(), Some("`${this.url}/${id}`"));
}

#[test]
fn resources_are_indexed_by_name() {
    assert_eq!(
        Lang::from_path("src/main/resources/application-dev.yml"),
        Some(Lang::Yaml)
    );
    assert_eq!(
        Lang::from_path("src/main/resources/bootstrap.properties"),
        Some(Lang::Properties)
    );
    assert_eq!(Lang::from_path("api/openapi.yaml"), Some(Lang::Yaml));
    assert_eq!(Lang::from_path(".github/workflows/ci.yml"), None);
    assert_eq!(Lang::from_path("i18n/messages.properties"), None);
    let f = parse("src/app/x.component.html", "<app-x></app-x>\n");
    assert_eq!(f.elements[0].name, "app-x");
    assert!(f.defs.is_empty());
    let c = parse("src/main/resources/application.properties", "a.b=1\n");
    assert_eq!(c.config[0].key, "a.b");
}

#[test]
fn merge_prefixes_the_other_index() {
    let mk = |paths: &[&str]| Index {
        files: paths.iter().map(|p| parse(p, "class A {}\n")).collect(),
        base_files: Vec::new(),
        stats: IndexStats {
            files: paths.len(),
            ..Default::default()
        },
    };
    let mut front = mk(&["src/a.ts", "src/z.ts"]);
    front.merge(mk(&["src/Main.java"]), "back/");
    let paths: Vec<&str> = front.files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(paths, ["back/src/Main.java", "src/a.ts", "src/z.ts"]);
    assert!(front.file("back/src/Main.java").is_some());
    assert_eq!(front.stats.files, 3);
}
