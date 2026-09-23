//! Flows on inline Angular + Spring sources.

use super::*;
use crate::graph::{GraphData, build, default_rules};
use crate::index::{Index, Lang, parse_file};

fn graph(files: &[(&str, &str)]) -> Graph {
    let mut v: Vec<_> = files
        .iter()
        .map(|(p, s)| parse_file(p, Lang::from_path(p).expect("indexed path"), s))
        .collect();
    v.sort_by(|a, b| a.path.cmp(&b.path));
    let index = Index {
        files: v,
        base_files: Vec::new(),
        stats: Default::default(),
    };
    build(&index, &Default::default(), &default_rules())
}

/// The symbol displayed as `name` (constructors aside).
fn sym(g: &Graph, name: &str) -> SymIdx {
    let found: Vec<SymIdx> = (0..g.symbols().len() as SymIdx)
        .filter(|&s| g.display_name(s) == name && g.symbol(s).kind != SymbolKind::Constructor)
        .collect();
    assert_eq!(found.len(), 1, "{name}: {found:?}");
    found[0]
}

/// `g` with `names` marked changed, and their files (plus `files`) in the diff.
fn changing(g: &Graph, names: &[&str], files: &[&str]) -> Graph {
    let syms: Vec<SymIdx> = names.iter().map(|n| sym(g, n)).collect();
    let mut data: GraphData = g.data().clone();
    let mut paths: Vec<String> = files.iter().map(|f| f.to_string()).collect();
    for s in syms {
        data.symbols[s as usize].changed = true;
        paths.push(data.symbols[s as usize].file.clone());
    }
    for f in &mut data.files {
        f.changed |= paths.contains(&f.path);
    }
    Graph::new(data)
}

/// `depth via name` of each step.
fn outline(steps: &[Step]) -> Vec<String> {
    steps
        .iter()
        .map(|s| format!("{}{} {}", "  ".repeat(s.depth), s.via.label(), s.name))
        .collect()
}

const ROUTES: &str = r#"import { Routes } from '@angular/router';
import { OwnerDetailComponent } from './owner-detail.component';
export const routes: Routes = [{ path: 'owners/:id', component: OwnerDetailComponent }];
"#;
const DETAIL: &str = r#"import { Component } from '@angular/core';
import { OwnerService } from './owner.service';
@Component({ selector: 'app-owner-detail', template: '<p>owner</p>' })
export class OwnerDetailComponent {
  constructor(private owners: OwnerService) {}
  ngOnInit() { this.owners.get(1).subscribe(); }
  title() { return 'Owner'; }
}
"#;
const FRONT_SERVICE: &str = r#"import { Injectable } from '@angular/core';
import { HttpClient } from '@angular/common/http';
import { environment } from '../environments/environment';
@Injectable({ providedIn: 'root' })
export class OwnerService {
  private readonly base = environment.apiUrl + 'owners';
  constructor(private http: HttpClient) {}
  get(id: number) { return this.http.get(`${this.base}/${id}`); }
}
"#;
const ENVIRONMENT: &str =
    "export const environment = {\n  apiUrl: 'http://localhost:8080/api/'\n};\n";
const CONTROLLER: &str = r#"package app.owners;
@RestController
@RequestMapping("/api/owners")
public class OwnerRestController {
    private final OwnerManager manager;
    public OwnerRestController(OwnerManager manager) { this.manager = manager; }
    @GetMapping("/{ownerId}")
    public Owner get(@PathVariable int ownerId) { return manager.find(ownerId); }
    @GetMapping("/count")
    public int count() { return manager.count(); }
}
"#;
const MANAGER: &str = r#"package app.owners;
public interface OwnerManager {
    Owner find(int id);
    int count();
}
"#;
const MANAGER_IMPL: &str = r#"package app.owners;
@Service
public class OwnerManagerImpl implements OwnerManager {
    private final OwnerRepository repo;
    private final AuditClient audit;
    public OwnerManagerImpl(OwnerRepository repo, AuditClient audit) { this.repo = repo; this.audit = audit; }
    @Override
    public Owner find(int id) {
        audit.record(id);
        return repo.findById(id);
    }
    @Override
    public int count() { return 3; }
}
"#;
/// Before: `find` went through a legacy DAO, without audit.
const MANAGER_IMPL_BASE: &str = r#"package app.owners;
@Service
public class OwnerManagerImpl implements OwnerManager {
    private final LegacyOwnerDao dao;
    public OwnerManagerImpl(LegacyOwnerDao dao) { this.dao = dao; }
    @Override
    public Owner find(int id) {
        return dao.load(id);
    }
    @Override
    public int count() { return 3; }
}
"#;
const LEGACY_DAO: &str = r#"package app.owners;
@Repository
public class LegacyOwnerDao {
    public Owner load(int id) { return null; }
}
"#;
const REPOSITORY: &str = r#"package app.owners;
public interface OwnerRepository extends JpaRepository<Owner, Integer> {
    Owner findById(int id);
}
"#;
const AUDIT: &str = r#"package app.audit;
@Component
public class AuditClient {
    private final RestTemplate rest;
    public AuditClient(RestTemplate rest) { this.rest = rest; }
    public void record(int id) { rest.postForObject("https://audit.example/owners", id, Void.class); }
}
"#;
const OWNER: &str = r#"package app.owners;
@Entity
@Table(name = "owners")
public class Owner {
    private int id;
    public int getId() { return id; }
}
"#;

fn front() -> Vec<(&'static str, &'static str)> {
    vec![
        ("web/src/app/app.routes.ts", ROUTES),
        ("web/src/app/owner-detail.component.ts", DETAIL),
        ("web/src/app/owner.service.ts", FRONT_SERVICE),
        ("web/src/environments/environment.ts", ENVIRONMENT),
    ]
}

const IMPL_PATH: &str = "api/src/main/java/app/owners/OwnerManagerImpl.java";

fn head_files() -> Vec<(&'static str, &'static str)> {
    let mut v = front();
    v.extend([
        (
            "api/src/main/java/app/owners/OwnerRestController.java",
            CONTROLLER,
        ),
        ("api/src/main/java/app/owners/OwnerManager.java", MANAGER),
        (IMPL_PATH, MANAGER_IMPL),
        (
            "api/src/main/java/app/owners/OwnerRepository.java",
            REPOSITORY,
        ),
        ("api/src/main/java/app/audit/AuditClient.java", AUDIT),
        ("api/src/main/java/app/owners/Owner.java", OWNER),
    ]);
    v
}

fn base_files() -> Vec<(&'static str, &'static str)> {
    let mut v = front();
    v.extend([
        (
            "api/src/main/java/app/owners/OwnerRestController.java",
            CONTROLLER,
        ),
        ("api/src/main/java/app/owners/OwnerManager.java", MANAGER),
        (IMPL_PATH, MANAGER_IMPL_BASE),
        (
            "api/src/main/java/app/owners/LegacyOwnerDao.java",
            LEGACY_DAO,
        ),
        (
            "api/src/main/java/app/owners/OwnerRepository.java",
            REPOSITORY,
        ),
        ("api/src/main/java/app/owners/Owner.java", OWNER),
    ]);
    v
}

#[test]
fn front_to_back_flow_reaches_persistence_and_external_calls() {
    let g = changing(&graph(&head_files()), &["OwnerManagerImpl.find"], &[]);
    let flows = impacted(&g, &Limits::default());
    let entries: Vec<(EntryKind, &str)> = flows
        .iter()
        .map(|f| (f.entry.kind, f.entry.label.as_str()))
        .collect();
    // The route first, then the endpoint; `count` does not reach the change.
    assert_eq!(
        entries,
        [
            (EntryKind::Route, "/owners/:id"),
            (EntryKind::Http, "GET /api/owners/{ownerId}"),
        ]
    );
    let route = flows[0].after.as_ref().unwrap();
    assert_eq!(
        outline(&route.steps),
        [
            "entry OwnerDetailComponent",
            "  member OwnerDetailComponent.ngOnInit",
            "    calls OwnerService.get",
            "      http OwnerRestController.get",
            "        calls OwnerManager.find",
            "          impl OwnerManagerImpl.find",
            "            calls AuditClient.record",
            "            calls OwnerRepository.findById",
            "              persists Owner",
        ]
    );
    let step = |name: &str| route.steps.iter().find(|s| s.name == name).unwrap();
    // `title` reaches nothing: pruned.
    assert_eq!(step("OwnerDetailComponent").hidden, 1);
    assert_eq!(step("OwnerRestController.get").layer, Layer::Controller);
    assert_eq!(step("OwnerManager.find").layer, Layer::Service);
    assert_eq!(
        step("OwnerRestController.get").detail.as_deref(),
        Some("GET /api/owners/{ownerId}")
    );
    assert_eq!(
        step("AuditClient.record").terminal,
        Some(Terminal::External)
    );
    assert_eq!(
        step("OwnerRepository.findById").terminal,
        Some(Terminal::Persistence)
    );
    assert_eq!(step("Owner").terminal, Some(Terminal::Entity));
    assert!(step("OwnerManagerImpl.find").changed);
    assert!(step("OwnerService.get").reaches_changed);
    assert!(!step("OwnerRepository.findById").reaches_changed);
    // Path confidence: product of the edges from the entry.
    let mut product = 1.0;
    for s in &route.steps[..6] {
        product *= s.edge_confidence;
    }
    assert!((step("OwnerManagerImpl.find").confidence - product).abs() < 1e-5);
    assert!(route.confidence < 1.0 && route.confidence > 0.3);
    assert_eq!(route.changed_steps, 1);
    // The endpoint's own flow starts at the controller.
    assert_eq!(
        flows[1].after.as_ref().unwrap().steps[0].name,
        "OwnerRestController.get"
    );
    assert_eq!(find(&flows, "owners/:id").len(), 1);
}

#[test]
fn a_changed_entry_is_impacted_and_low_confidence_paths_are_cut() {
    let g = changing(&graph(&head_files()), &["OwnerRestController.get"], &[]);
    let flows = impacted(&g, &Limits::default());
    assert!(
        flows
            .iter()
            .any(|f| f.entry.changed && f.entry.kind == EntryKind::Http)
    );
    // Every HTTP link of the fixture is below 0.95: the route no longer
    // reaches the endpoint.
    let strict = Limits {
        min_confidence: 0.95,
        ..Limits::default()
    };
    let flows = impacted(&g, &strict);
    assert_eq!(flows.len(), 1);
    assert_eq!(flows[0].entry.kind, EntryKind::Http);
}

#[test]
fn cycles_and_repeats_are_not_expanded() {
    let g = graph(&[(
        "src/main/java/app/Loop.java",
        r#"package app;
@RestController
public class Loop {
    @GetMapping("/loop")
    public void start() { ping(); pong(); }
    void ping() { pong(); }
    void pong() { ping(); leaf(); }
    void leaf() { }
}
"#,
    )]);
    let g = changing(&g, &["Loop.leaf"], &[]);
    let flows = impacted(&g, &Limits::default());
    assert_eq!(flows.len(), 1);
    let steps = &flows[0].after.as_ref().unwrap().steps;
    assert_eq!(
        outline(steps),
        [
            "entry Loop.start",
            "  calls Loop.ping",
            "    calls Loop.pong",
            "      calls Loop.ping",
            "      calls Loop.leaf",
            "  calls Loop.pong",
        ]
    );
    assert_eq!(steps[3].repeat, Some(Repeat::Cycle));
    assert_eq!(steps[5].repeat, Some(Repeat::Seen));
    assert!(steps[5].reaches_changed);

    // Depth limit: cut, and said so.
    let shallow = Limits {
        max_depth: 1,
        ..Limits::default()
    };
    let f = flow_from(
        &g,
        sym(&g, "Loop.start"),
        &|s| g.symbol(s).changed,
        &shallow,
    );
    assert!(f.truncated);
    assert!(f.steps.iter().all(|s| s.depth <= 1));
}

#[test]
fn before_after_shows_reroutes_new_external_calls_and_persistence() {
    let head = changing(
        &graph(&head_files()),
        &["OwnerManagerImpl.find", "OwnerManagerImpl"],
        &[],
    );
    let base = graph(&base_files());
    let limits = Limits::default();
    let mut flows = impacted(&head, &limits);
    compare(&mut flows, &head, &base, &limits);
    let http = flows
        .iter()
        .find(|f| f.entry.kind == EntryKind::Http)
        .unwrap();
    let d = http.diff.as_ref().unwrap();
    assert_eq!(d.status, FlowStatus::Changed);
    assert!(d.is_relevant());
    let names = |v: &[StepRef]| v.iter().map(|r| r.name.clone()).collect::<Vec<_>>();
    assert_eq!(
        names(&d.added),
        ["AuditClient.record", "OwnerRepository.findById"]
    );
    assert_eq!(names(&d.removed), ["LegacyOwnerDao.load"]);
    assert_eq!(names(&d.new_external), ["AuditClient.record"]);
    assert_eq!(names(&d.new_persistence), ["OwnerRepository.findById"]);
    assert_eq!(names(&d.dropped_persistence), ["LegacyOwnerDao.load"]);
    // Owner is reached from another repository: not a reroute.
    assert!(d.rerouted.is_empty());
    let merged: Vec<String> = d
        .steps
        .iter()
        .map(|s| format!("{:?} {}", s.change, s.step.name))
        .collect();
    assert_eq!(
        merged,
        [
            "Same OwnerRestController.get",
            "Same OwnerManager.find",
            "Same OwnerManagerImpl.find",
            "Removed LegacyOwnerDao.load",
            "Removed Owner",
            "Added AuditClient.record",
            "Added OwnerRepository.findById",
            "Added Owner",
        ]
    );
    // The base flow knows what the review changes, by id.
    let before = http.before.as_ref().unwrap();
    assert!(
        before
            .steps
            .iter()
            .any(|s| s.name == "OwnerManagerImpl.find" && s.changed)
    );
    assert!(d.summary().contains("1 new external call(s)"));
    let mermaid = d.to_mermaid(&http.entry.label);
    assert!(mermaid.starts_with("---\ntitle: GET /api/owners/{ownerId} (before / after)"));
    assert!(mermaid.contains("-.->|persists"), "{mermaid}");
    assert!(mermaid.contains("==>|calls"), "{mermaid}");
    assert!(mermaid.contains("{{\"AuditClient.record"), "{mermaid}");
    assert!(mermaid.contains("class n5 added"), "{mermaid}");

    // Nothing differs: not relevant.
    let same = diff_flows(http.after.as_ref(), http.after.as_ref());
    assert_eq!(same.status, FlowStatus::Same);
    assert!(!same.is_relevant());
}

#[test]
fn new_and_removed_entry_points() {
    let limits = Limits::default();
    // Base: an extra endpoint in the controller, gone in the head.
    let base_controller = CONTROLLER.replace(
        "    @GetMapping(\"/count\")",
        "    @DeleteMapping(\"/{ownerId}\")\n    public void drop(@PathVariable int ownerId) { manager.find(ownerId); }\n    @GetMapping(\"/count\")",
    );
    let mut base_src = base_files();
    base_src[4].1 = &base_controller;
    let base = graph(&base_src);
    let head = changing(
        &graph(&head_files()),
        &["OwnerManagerImpl.find"],
        &["api/src/main/java/app/owners/OwnerRestController.java"],
    );
    let mut flows = impacted(&head, &limits);
    compare(&mut flows, &head, &base, &limits);
    let gone = flows
        .iter()
        .find(|f| f.entry.label == "DELETE /api/owners/{ownerId}")
        .expect("removed endpoint listed");
    assert!(gone.after.is_none());
    assert_eq!(gone.diff.as_ref().unwrap().status, FlowStatus::Removed);
    assert!(
        gone.diff
            .as_ref()
            .unwrap()
            .steps
            .iter()
            .all(|s| s.change == Change::Removed)
    );

    // An entry missing from the base is new.
    let d = diff_flows(None, flows[0].after.as_ref());
    assert_eq!(d.status, FlowStatus::New);
    assert!(d.steps.iter().all(|s| s.change == Change::Added));
}

#[test]
fn flow_mermaid_marks_changes_and_terminals() {
    let g = changing(&graph(&head_files()), &["OwnerManagerImpl.find"], &[]);
    let flows = impacted(&g, &Limits::default());
    let m = flows[1].after.as_ref().unwrap().to_mermaid();
    assert!(m.contains("flowchart TD"));
    assert!(m.contains("n0([\"OwnerRestController.get"), "{m}");
    assert!(m.contains("[(\"OwnerRepository.findById"), "{m}");
    assert!(m.contains("class n2 changed"), "{m}");
    assert!(m.contains("-->|impl"), "{m}");
    // Without a base, the impacted flow exports the flow itself.
    assert_eq!(flows[1].to_mermaid(), m);
}
