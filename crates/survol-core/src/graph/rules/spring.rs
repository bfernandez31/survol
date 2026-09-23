//! Spring (Java / Kotlin): entry points, components, injection, persistence,
//! external calls, configuration and application events.
//!
//! - Entry points ([`Role::EntryPoint`], tag `entry`): `@*Mapping` methods of
//!   `@Controller` / `@RestController` classes (class-level prefix combined,
//!   mappings inherited from an implemented interface, OpenAPI operations
//!   implemented by a controller of a generated `*Api`), `@KafkaListener`,
//!   `@RabbitListener`, `@JmsListener`, `@SqsListener`, `@Scheduled`,
//!   `@EventListener`, `ApplicationListener`, `CommandLineRunner` /
//!   `ApplicationRunner`, `main` of the `@SpringBootApplication`.
//! - Components: stereotypes and `@Bean` methods, with their bean names.
//! - Injection (`injects` edges, consumer → implementation): constructors
//!   (single, or `@Autowired`), Kotlin primary constructors and records,
//!   Lombok `@RequiredArgsConstructor` / `@AllArgsConstructor` fields,
//!   `@Autowired` / `@Inject` / `@Resource` fields and setters, `@Bean`
//!   method parameters. Interface → implementations and `@Bean` producers;
//!   `@Qualifier` / `@Named`, `@Primary` and the parameter name narrow.
//! - Persistence: Spring Data repositories (→ entity from the generic
//!   argument), `@Repository`, `@Entity` / `@Document` / `@Embeddable`...
//! - External calls: `@FeignClient` ([`Role::External`], `http.calls` on
//!   its methods), `RestTemplate` / `WebClient` / `RestClient` users.
//! - Configuration: `application*.yml|properties` keys → `@Value("${…}")`,
//!   `@ConfigurationProperties(prefix)` and placeholders of `@FeignClient`
//!   (`configures` edges, file → consumer).
//! - Events: `publishEvent(X)` → listeners of `X` or a supertype
//!   (`publishes` edges).

use std::collections::{HashMap, HashSet};

use super::FrameworkRule;
use super::util::{
    Part, add_tag, annotation, binding_at, class_literals, decapitalize, eval, has_annotation,
    members, module_root, placeholders, receiver_name, relaxed_key, split_top,
};
use crate::graph::{EdgeKind, GraphBuilder, Role, SymIdx, SymbolKind};
use crate::index::{Annotation, AnnotationArg, Def, FileIndex, Index, Lang, RefKind, unquote};

pub struct Spring;

const STEREOTYPES: &[&str] = &[
    "Component",
    "Service",
    "Repository",
    "Controller",
    "RestController",
    "Configuration",
    "ControllerAdvice",
    "RestControllerAdvice",
    "SpringBootApplication",
    "Named",
    "ManagedBean",
];
const CONTROLLERS: &[&str] = &["Controller", "RestController"];
const CONFIGURATIONS: &[&str] = &["Configuration", "SpringBootApplication"];
const INJECT: &[&str] = &["Autowired", "Inject", "Resource"];
const QUALIFIERS: &[&str] = &["Qualifier", "Named"];
const MAPPINGS: &[(&str, &str)] = &[
    ("GetMapping", "GET"),
    ("PostMapping", "POST"),
    ("PutMapping", "PUT"),
    ("DeleteMapping", "DELETE"),
    ("PatchMapping", "PATCH"),
    ("RequestMapping", ""),
];
const HTTP_METHODS: &[&str] = &["GET", "POST", "PUT", "DELETE", "PATCH", "HEAD", "OPTIONS"];
/// Spring Data base interfaces: extending one makes a repository.
const DATA_REPOSITORIES: &[&str] = &[
    "Repository",
    "CrudRepository",
    "ListCrudRepository",
    "PagingAndSortingRepository",
    "ListPagingAndSortingRepository",
    "JpaRepository",
    "MongoRepository",
    "ReactiveCrudRepository",
    "ReactiveSortingRepository",
    "ReactiveMongoRepository",
    "R2dbcRepository",
    "CoroutineCrudRepository",
    "CoroutineSortingRepository",
    "ElasticsearchRepository",
    "Neo4jRepository",
    "CassandraRepository",
    "KeyValueRepository",
];
const ENTITIES: &[&str] = &[
    "Entity",
    "Document",
    "Embeddable",
    "MappedSuperclass",
    "Table",
];
/// (annotation, `entry` tag, tag key, argument names holding the targets).
const LISTENERS: &[(&str, &str, &str, &[&str])] = &[
    (
        "KafkaListener",
        "kafka",
        "kafka.topics",
        &["topics", "topicPattern", "value"],
    ),
    (
        "RabbitListener",
        "rabbit",
        "rabbit.queues",
        &["queues", "queuesToDeclare", "value"],
    ),
    (
        "JmsListener",
        "jms",
        "jms.destination",
        &["destination", "value"],
    ),
    ("SqsListener", "sqs", "sqs.queues", &["queueNames", "value"]),
    (
        "Scheduled",
        "scheduled",
        "schedule",
        &[
            "cron",
            "fixedRate",
            "fixedDelay",
            "fixedRateString",
            "fixedDelayString",
            "value",
        ],
    ),
    ("EventListener", "event", "event.type", &[]),
    ("TransactionalEventListener", "event", "event.type", &[]),
];
const RUNNERS: &[&str] = &["CommandLineRunner", "ApplicationRunner"];
/// HTTP client types and their `external` tag.
pub const HTTP_CLIENTS: &[(&str, &str)] = &[
    ("RestTemplate", "rest_template"),
    ("WebClient", "web_client"),
    ("RestClient", "rest_client"),
];
/// Injected types that are never beans of the project.
const NOT_BEANS: &[&str] = &[
    "String", "Integer", "Long", "Boolean", "Double", "Int", "Duration", "Object", "Any",
];

/// Confidence of an injection resolved to one bean.
const INJECT_ONE: f32 = 0.9;
/// Confidence of a `configures` edge to a key of the same module.
const CONFIG_SAME_MODULE: f32 = 0.9;
const CONFIG_OTHER_MODULE: f32 = 0.6;

impl FrameworkRule for Spring {
    fn name(&self) -> &str {
        "spring"
    }

    fn apply(&self, index: &Index, g: &mut GraphBuilder) {
        let files: Vec<usize> = (0..index.files.len())
            .filter(|&fi| {
                index.files[fi].lang.is_jvm() && !g.symbol(g.file_symbol(fi)).has_role(Role::Test)
            })
            .collect();
        if files.is_empty() {
            return;
        }
        let mut cx = Cx {
            index,
            files,
            beans: HashMap::new(),
            producers: HashMap::new(),
            listeners: Vec::new(),
            configs: config_files(index, g),
            overrides: HashMap::new(),
        };
        for e in g.edges() {
            if e.kind == EdgeKind::Overrides {
                cx.overrides.entry(e.from).or_default().push(e.to);
            }
        }
        cx.components(g);
        cx.persistence(g);
        cx.web(g);
        cx.openapi(g);
        cx.entry_points(g);
        cx.injection(g);
        cx.configuration(g);
        cx.events(g);
        cx.http_clients(g);
    }
}

/// A bean: class or `@Bean` method.
#[derive(Debug, Clone)]
struct Bean {
    names: Vec<String>,
    primary: bool,
}

/// A configuration file with its relaxed keys.
struct ConfigFile {
    file: usize,
    root: String,
    /// relaxed key → (key as written, value, line)
    keys: Vec<(String, String, Option<String>, u32)>,
}

struct Cx<'a> {
    index: &'a Index,
    /// Main (non-test) Java / Kotlin files.
    files: Vec<usize>,
    /// Bean classes.
    beans: HashMap<SymIdx, Bean>,
    /// Type name → `@Bean` methods producing it.
    producers: HashMap<String, Vec<(SymIdx, Bean)>>,
    /// Event listener methods with their event type names (and file).
    listeners: Vec<(SymIdx, usize, Vec<String>)>,
    configs: Vec<ConfigFile>,
    /// Method → the methods it overrides.
    overrides: HashMap<SymIdx, Vec<SymIdx>>,
}

fn config_files(index: &Index, g: &mut GraphBuilder) -> Vec<ConfigFile> {
    let mut out = Vec::new();
    for (fi, f) in index.files.iter().enumerate() {
        let s = g.file_symbol(fi);
        // Test configuration only configures tests.
        if !matches!(f.lang, Lang::Yaml | Lang::Properties)
            || !crate::index::is_config_name(&f.path)
            || g.symbol(s).has_role(Role::Test)
        {
            continue;
        }
        g.add_role(s, Role::Config);
        let name = f.path.rsplit('/').next().unwrap_or(&f.path);
        let stem = name.split('.').next().unwrap_or(name);
        if let Some((_, profile)) = stem.split_once('-') {
            add_tag(g, s, "spring.profile", profile);
        }
        out.push(ConfigFile {
            file: fi,
            root: module_root(&f.path).to_string(),
            keys: f
                .config
                .iter()
                .map(|e| (relaxed_key(&e.key), e.key.clone(), e.value.clone(), e.line))
                .collect(),
        });
    }
    out
}

impl<'a> Cx<'a> {
    fn file(&self, fi: usize) -> &'a FileIndex {
        &self.index.files[fi]
    }

    /// Main JVM type definitions: (file, def index, def).
    fn types(&self) -> Vec<(usize, usize, &'a Def)> {
        let index = self.index;
        self.files
            .iter()
            .flat_map(|&fi| {
                index.files[fi]
                    .defs
                    .iter()
                    .enumerate()
                    .filter(|(_, d)| d.kind.is_type())
                    .map(move |(d, def)| (fi, d, def))
            })
            .collect()
    }

    /// Names of the supertypes written on type `d` (extends / implements).
    fn super_names(&self, fi: usize, d: usize) -> Vec<&'a str> {
        self.file(fi)
            .refs
            .iter()
            .filter(|r| {
                matches!(r.kind, RefKind::Extends | RefKind::Implements)
                    && r.scope == Some(d as u32)
            })
            .map(|r| r.name.as_str())
            .collect()
    }

    /// First type argument after the supertype `name` of type `d`:
    /// `extends JpaRepository<Owner, Integer>` → `Owner`.
    fn type_argument(&self, fi: usize, d: usize, name: &str) -> Option<&'a str> {
        let refs = &self.file(fi).refs;
        let i = refs.iter().position(|r| {
            matches!(r.kind, RefKind::Extends | RefKind::Implements)
                && r.scope == Some(d as u32)
                && r.name == name
        })?;
        let line = refs[i].line;
        refs[i + 1..]
            .iter()
            .take_while(|r| r.line <= line + 1)
            .find(|r| r.kind == RefKind::Type && r.scope == Some(d as u32) && r.name != name)
            .map(|r| r.name.as_str())
    }

    // ---- components

    fn components(&mut self, g: &mut GraphBuilder) {
        for (fi, d, def) in self.types() {
            let s = g.def_symbol(fi, d);
            let Some(st) = annotation(def, STEREOTYPES) else {
                continue;
            };
            g.add_role(s, Role::Component);
            if has_annotation(&def.annotations, CONFIGURATIONS) {
                g.add_role(s, Role::Config);
            }
            if st.name == "Repository" {
                g.add_role(s, Role::Repository);
            }
            add_tag(g, s, "spring.stereotype", &st.name);
            let mut names: Vec<String> = def
                .annotations
                .iter()
                .filter(|a| {
                    STEREOTYPES.contains(&a.name.as_str()) || QUALIFIERS.contains(&a.name.as_str())
                })
                .filter_map(|a| a.value().and_then(AnnotationArg::as_str))
                .map(str::to_string)
                .collect();
            names.push(decapitalize(&def.name));
            add_tag(g, s, "spring.bean", &names[0]);
            let primary = def.annotation("Primary").is_some();
            self.beans.insert(s, Bean { names, primary });
            // `@Bean` methods.
            for (m, mdef) in members(self.file(fi), d) {
                let Some(bean) = mdef.annotation("Bean") else {
                    continue;
                };
                let ms = g.def_symbol(fi, m);
                g.add_role(ms, Role::Config);
                g.add_role(ms, Role::Component);
                let mut names: Vec<String> = bean
                    .args
                    .iter()
                    .filter(|a| {
                        a.key.is_none() || matches!(a.key.as_deref(), Some("name" | "value"))
                    })
                    .flat_map(|a| a.strings())
                    .map(str::to_string)
                    .collect();
                names.extend(
                    mdef.annotations
                        .iter()
                        .filter(|a| QUALIFIERS.contains(&a.name.as_str()))
                        .filter_map(|a| a.value().and_then(AnnotationArg::as_str))
                        .map(str::to_string),
                );
                names.push(mdef.name.clone());
                add_tag(g, ms, "spring.bean", &names[0]);
                if let Some(t) = &mdef.type_name {
                    add_tag(g, ms, "bean.type", t);
                    self.producers.entry(t.clone()).or_default().push((
                        ms,
                        Bean {
                            names,
                            primary: mdef.annotation("Primary").is_some(),
                        },
                    ));
                }
            }
        }
    }

    // ---- persistence

    fn persistence(&mut self, g: &mut GraphBuilder) {
        let mut repos: Vec<(usize, usize, SymIdx)> = Vec::new();
        for (fi, d, def) in self.types() {
            let s = g.def_symbol(fi, d);
            if let Some(a) = annotation(def, ENTITIES) {
                g.add_role(s, Role::Entity);
                if let Some(t) = def
                    .annotation("Table")
                    .and_then(|t| t.arg("name").or(t.value()))
                    .and_then(AnnotationArg::as_str)
                {
                    add_tag(g, s, "persistence.table", t);
                }
                let _ = a;
            }
            if def.kind != SymbolKind::Interface {
                continue;
            }
            for sup in self.super_names(fi, d) {
                if DATA_REPOSITORIES.contains(&sup) && g.resolve_type(fi, sup).is_empty() {
                    repos.push((fi, d, s));
                    g.add_role(s, Role::Repository);
                    add_tag(g, s, "persistence", "spring_data");
                    if let Some(entity) = self.type_argument(fi, d, sup) {
                        add_tag(g, s, "persistence.entity", entity);
                        let line = def.line;
                        for (t, c) in g.resolve_type(fi, entity) {
                            g.add_edge(s, t, EdgeKind::Uses, 0.95 * c, line);
                        }
                    }
                }
            }
        }
        // Interfaces extending a project repository are repositories too.
        let mut frontier: Vec<SymIdx> = repos.iter().map(|r| r.2).collect();
        let mut seen: HashSet<SymIdx> = frontier.iter().copied().collect();
        while let Some(r) = frontier.pop() {
            let subs: Vec<SymIdx> = g.subtypes(r).iter().map(|&(s, _)| s).collect();
            for s in subs {
                if g.symbol(s).kind == SymbolKind::Interface
                    && !g.symbol(s).has_role(Role::Test)
                    && seen.insert(s)
                {
                    g.add_role(s, Role::Repository);
                    add_tag(g, s, "persistence", "spring_data");
                    frontier.push(s);
                }
            }
        }
    }

    // ---- web

    /// `@*Mapping` of a method: (HTTP methods, paths).
    fn mapping(
        &self,
        g: &GraphBuilder,
        fi: usize,
        def: &Def,
    ) -> Option<(Vec<String>, Vec<String>)> {
        MAPPINGS.iter().find_map(|(name, verb)| {
            let a = def.annotation(name)?;
            let verbs = if verb.is_empty() {
                let m: Vec<String> = a
                    .arg("method")
                    .map(|m| {
                        HTTP_METHODS
                            .iter()
                            .filter(|v| {
                                m.value
                                    .split(|c: char| !c.is_ascii_alphabetic())
                                    .any(|w| w == **v)
                            })
                            .map(|v| v.to_string())
                            .collect()
                    })
                    .unwrap_or_default();
                if m.is_empty() { vec!["ANY".into()] } else { m }
            } else {
                vec![verb.to_string()]
            };
            Some((verbs, self.paths(g, fi, a)))
        })
    }

    /// Paths of a mapping annotation (`value`, `path` or positional), with
    /// constants resolved; `[""]` when none.
    fn paths(&self, g: &GraphBuilder, fi: usize, a: &Annotation) -> Vec<String> {
        // Kotlin allows several positional values (`@GetMapping("/a", "/b")`).
        let mut args: Vec<&AnnotationArg> = a.args.iter().filter(|x| x.key.is_none()).collect();
        if args.is_empty() {
            args.extend(a.arg("path").or_else(|| a.arg("value")));
        }
        if args.is_empty() {
            return vec![String::new()];
        }
        args.iter()
            .flat_map(|x| annotation_strings(g, fi, &x.value))
            .collect()
    }

    fn web(&mut self, g: &mut GraphBuilder) {
        for (fi, d, def) in self.types() {
            let controller = has_annotation(&def.annotations, CONTROLLERS);
            let feign = def.annotation("FeignClient");
            if !controller && feign.is_none() {
                continue;
            }
            let cls = g.def_symbol(fi, d);
            let mut prefixes = match def.annotation("RequestMapping") {
                Some(a) => self.paths(g, fi, a),
                None => vec![String::new()],
            };
            if let Some(f) = feign {
                g.add_role(cls, Role::External);
                g.add_role(cls, Role::Component);
                add_tag(g, cls, "external", "feign");
                if let Some(n) = f
                    .arg("name")
                    .or_else(|| f.arg("value"))
                    .or_else(|| f.args.iter().find(|a| a.key.is_none()))
                    .and_then(AnnotationArg::as_str)
                {
                    add_tag(g, cls, "http.client", n);
                }
                if let Some(u) = f.arg("url").and_then(AnnotationArg::as_str) {
                    add_tag(g, cls, "http.base_url", u);
                }
                if let Some(p) = f.arg("path") {
                    prefixes = annotation_strings(g, fi, &p.value);
                }
            }
            let context = self.context_path(&self.file(fi).path);
            for (m, mdef) in members(self.file(fi), d) {
                if !mdef.kind.is_callable() {
                    continue;
                }
                let ms = g.def_symbol(fi, m);
                let (verbs, paths, class_prefixes) = match self.mapping(g, fi, mdef) {
                    Some((v, p)) => (v, p, prefixes.clone()),
                    None if controller => match self.inherited_mapping(g, ms) {
                        Some(x) => x,
                        None => continue,
                    },
                    None => continue,
                };
                let class_prefixes = if def.annotation("RequestMapping").is_some() {
                    prefixes.clone()
                } else {
                    class_prefixes
                };
                for verb in &verbs {
                    for prefix in &class_prefixes {
                        for path in &paths {
                            let route = format!("{verb} {}", join_path(prefix, path));
                            if controller {
                                add_tag(g, ms, "http.route", &route);
                            } else {
                                add_tag(g, ms, "http.calls", &route);
                            }
                        }
                    }
                }
                if controller {
                    g.add_role(ms, Role::EntryPoint);
                    add_tag(g, ms, "entry", "http");
                    if let Some(c) = &context {
                        add_tag(g, ms, "http.context_path", c);
                    }
                } else {
                    g.add_role(ms, Role::External);
                }
            }
        }
    }

    /// Mapping of the method a controller method overrides (API interface),
    /// with the interface's class-level prefixes.
    fn inherited_mapping(
        &self,
        g: &GraphBuilder,
        m: SymIdx,
    ) -> Option<(Vec<String>, Vec<String>, Vec<String>)> {
        for &base in self.overrides.get(&m)? {
            let sym = g.symbol(base);
            let bfi = g.file_index(&sym.file)?;
            let bf = self.file(bfi);
            let bd = (0..bf.defs.len()).find(|&d| g.def_symbol(bfi, d) == base)?;
            if let Some((v, p)) = self.mapping(g, bfi, &bf.defs[bd]) {
                let prefixes = bf.defs[bd]
                    .parent
                    .and_then(|c| bf.defs[c as usize].annotation("RequestMapping"))
                    .map_or_else(|| vec![String::new()], |a| self.paths(g, bfi, a));
                return Some((v, p, prefixes));
            }
        }
        None
    }

    /// `server.servlet.context-path` (or equivalent) of the module of `path`.
    fn context_path(&self, path: &str) -> Option<String> {
        let root = module_root(path);
        let keys = [
            "server.servlet.contextpath",
            "server.contextpath",
            "spring.webflux.basepath",
        ];
        let mut found: Option<String> = None;
        for c in self
            .configs
            .iter()
            .filter(|c| c.root == root || root.starts_with(&c.root))
        {
            for (k, _, v, _) in &c.keys {
                if keys.contains(&k.as_str())
                    && let Some(v) = v
                {
                    // The default profile file wins over profile variants.
                    let name = self.file(c.file).path.rsplit('/').next().unwrap_or("");
                    let is_default = !name.contains('-');
                    if found.is_none() || is_default {
                        found = Some(join_path(v, ""));
                    }
                }
            }
        }
        found.filter(|c| c != "/")
    }

    /// OpenAPI operations → the controller methods implementing them.
    fn openapi(&mut self, g: &mut GraphBuilder) {
        let mut ops: HashMap<&str, Vec<(String, String, usize, u32)>> = HashMap::new();
        for (fi, f) in self.index.files.iter().enumerate() {
            if f.lang != Lang::Yaml
                || !f
                    .config
                    .iter()
                    .any(|e| e.key == "openapi" || e.key == "swagger")
            {
                continue;
            }
            for e in &f.config {
                let Some(rest) = e
                    .key
                    .strip_prefix("paths.")
                    .and_then(|k| k.strip_suffix(".operationId"))
                else {
                    continue;
                };
                let (Some((path, verb)), Some(op)) = (rest.rsplit_once('.'), e.value.as_deref())
                else {
                    continue;
                };
                let verb = verb.to_ascii_uppercase();
                if HTTP_METHODS.contains(&verb.as_str()) {
                    ops.entry(op)
                        .or_default()
                        .push((verb, path.to_string(), fi, e.line));
                }
            }
        }
        if ops.is_empty() {
            return;
        }
        for (fi, d, def) in self.types() {
            if !has_annotation(&def.annotations, CONTROLLERS) {
                continue;
            }
            // Implements an interface that is not in the index (generated).
            let generated = self
                .super_names(fi, d)
                .iter()
                .any(|n| g.resolve_type(fi, n).is_empty());
            if !generated {
                continue;
            }
            let prefixes = match def.annotation("RequestMapping") {
                Some(a) => self.paths(g, fi, a),
                None => vec![String::new()],
            };
            let context = self.context_path(&self.file(fi).path);
            for (m, mdef) in members(self.file(fi), d) {
                let Some(list) = ops.get(mdef.name.as_str()) else {
                    continue;
                };
                let ms = g.def_symbol(fi, m);
                if g.symbol(ms).tags.contains_key("http.route") {
                    continue;
                }
                for (verb, path, spec, line) in list {
                    for prefix in &prefixes {
                        add_tag(
                            g,
                            ms,
                            "http.route",
                            &format!("{verb} {}", join_path(prefix, path)),
                        );
                    }
                    let spec_sym = g.file_symbol(*spec);
                    g.add_edge(spec_sym, ms, EdgeKind::Configures, 0.8, *line);
                    add_tag(g, ms, "http.spec", &self.file(*spec).path);
                }
                g.add_role(ms, Role::EntryPoint);
                add_tag(g, ms, "entry", "http");
                if let Some(c) = &context {
                    add_tag(g, ms, "http.context_path", c);
                }
            }
        }
    }

    // ---- other entry points

    fn entry_points(&mut self, g: &mut GraphBuilder) {
        for (fi, d, def) in self.types() {
            let f = self.file(fi);
            let cls = g.def_symbol(fi, d);
            // Class-level listeners with `@KafkaHandler` / `@RabbitHandler` methods.
            let class_listener = LISTENERS
                .iter()
                .find(|l| def.annotation(l.0).is_some() && matches!(l.1, "kafka" | "rabbit"));
            for (m, mdef) in members(f, d) {
                if !mdef.kind.is_callable() {
                    continue;
                }
                let ms = g.def_symbol(fi, m);
                for &(ann, entry, key, args) in LISTENERS {
                    let a = match mdef.annotation(ann) {
                        Some(a) => a,
                        None => match class_listener {
                            Some(l)
                                if l.0 == ann
                                    && has_annotation(
                                        &mdef.annotations,
                                        &["KafkaHandler", "RabbitHandler"],
                                    ) =>
                            {
                                def.annotation(ann).expect("class listener")
                            }
                            _ => continue,
                        },
                    };
                    g.add_role(ms, Role::EntryPoint);
                    add_tag(g, ms, "entry", entry);
                    if entry == "event" {
                        let mut types: Vec<String> = a
                            .args
                            .iter()
                            .filter(|x| {
                                x.key.is_none()
                                    || matches!(x.key.as_deref(), Some("classes" | "value"))
                            })
                            .flat_map(|x| class_literals(&x.value))
                            .map(str::to_string)
                            .collect();
                        if types.is_empty() {
                            types.extend(
                                f.bindings
                                    .iter()
                                    .find(|b| b.scope == Some(m as u32))
                                    .map(|b| b.type_name.clone()),
                            );
                        }
                        for t in &types {
                            add_tag(g, ms, key, t);
                        }
                        self.listeners.push((ms, fi, types));
                        continue;
                    }
                    for x in a
                        .args
                        .iter()
                        .filter(|x| x.key.as_deref().is_none_or(|k| args.contains(&k)))
                    {
                        let v = x.strings();
                        if v.is_empty() {
                            add_tag(g, ms, key, x.value.trim());
                        }
                        for s in v {
                            add_tag(g, ms, key, s);
                        }
                    }
                }
                // `@Bean CommandLineRunner runner(...)`.
                if mdef.annotation("Bean").is_some()
                    && mdef
                        .type_name
                        .as_deref()
                        .is_some_and(|t| RUNNERS.contains(&t))
                {
                    g.add_role(ms, Role::EntryPoint);
                    add_tag(g, ms, "entry", "runner");
                }
            }
            let supers = self.super_names(fi, d);
            if supers.iter().any(|s| RUNNERS.contains(s)) {
                g.add_role(cls, Role::EntryPoint);
                add_tag(g, cls, "entry", "runner");
                for (m, mdef) in members(f, d) {
                    if mdef.name == "run" && mdef.kind.is_callable() {
                        let ms = g.def_symbol(fi, m);
                        g.add_role(ms, Role::EntryPoint);
                        add_tag(g, ms, "entry", "runner");
                    }
                }
            }
            if supers.contains(&"ApplicationListener") {
                let types: Vec<String> = self
                    .type_argument(fi, d, "ApplicationListener")
                    .map(str::to_string)
                    .into_iter()
                    .collect();
                for (m, mdef) in members(f, d) {
                    if mdef.name == "onApplicationEvent" {
                        let ms = g.def_symbol(fi, m);
                        g.add_role(ms, Role::EntryPoint);
                        add_tag(g, ms, "entry", "event");
                        for t in &types {
                            add_tag(g, ms, "event.type", t);
                        }
                        self.listeners.push((ms, fi, types.clone()));
                    }
                }
            }
            if def.annotation("SpringBootApplication").is_some() {
                // `main` in the class (Java) or at the top level of the file (Kotlin).
                let main = f.defs.iter().enumerate().find(|(_, x)| {
                    x.name == "main"
                        && x.kind.is_callable()
                        && (x.parent == Some(d as u32) || x.parent.is_none())
                });
                if let Some((m, _)) = main {
                    let ms = g.def_symbol(fi, m);
                    g.add_role(ms, Role::EntryPoint);
                    add_tag(g, ms, "entry", "main");
                }
            }
        }
    }

    // ---- injection

    fn injection(&mut self, g: &mut GraphBuilder) {
        let mut points: Vec<Point> = Vec::new();
        for (fi, d, def) in self.types() {
            let cls = g.def_symbol(fi, d);
            let is_bean = self.beans.contains_key(&cls);
            let f = self.file(fi);
            if is_bean {
                self.constructor_points(fi, d, def, cls, &mut points);
            }
            for (m, mdef) in members(f, d) {
                let ms = g.def_symbol(fi, m);
                let injected = has_annotation(&mdef.annotations, INJECT);
                match mdef.kind {
                    SymbolKind::Field if injected => {
                        if let Some(t) = &mdef.type_name {
                            points.push(Point {
                                from: cls,
                                fi,
                                type_name: t.clone(),
                                qualifier: qualifier(&mdef.annotations),
                                name: mdef.name.clone(),
                                line: mdef.line,
                            });
                        }
                    }
                    SymbolKind::Method
                        if injected || (is_bean && mdef.annotation("Bean").is_some()) =>
                    {
                        let from = if injected { cls } else { ms };
                        for b in params(f, m, mdef) {
                            if b.annotations.iter().any(|a| a.name == "Value") {
                                continue;
                            }
                            points.push(Point {
                                from,
                                fi,
                                type_name: b.type_name.clone(),
                                qualifier: qualifier(&b.annotations),
                                name: b.name.clone(),
                                line: b.line,
                            });
                        }
                    }
                    _ => {}
                }
            }
        }
        for p in points {
            if NOT_BEANS.contains(&p.type_name.as_str()) {
                continue;
            }
            let targets = self.inject_targets(g, &p);
            for (t, c) in targets {
                g.add_edge(p.from, t, EdgeKind::Injects, c, p.line);
            }
        }
    }

    /// Constructor injection points of bean class `d`.
    fn constructor_points(
        &self,
        fi: usize,
        d: usize,
        def: &Def,
        cls: SymIdx,
        out: &mut Vec<Point>,
    ) {
        let f = self.file(fi);
        let mut push = |type_name: &str, name: &str, annotations: &[Annotation], line: u32| {
            if annotations.iter().any(|a| a.name == "Value") {
                return;
            }
            out.push(Point {
                from: cls,
                fi,
                type_name: type_name.to_string(),
                qualifier: qualifier(annotations),
                name: name.to_string(),
                line,
            });
        };
        let ctors: Vec<(usize, &Def)> = members(f, d)
            .filter(|(_, x)| x.kind == SymbolKind::Constructor)
            .collect();
        let chosen: Vec<&(usize, &Def)> = if ctors.len() == 1 {
            ctors.iter().collect()
        } else {
            ctors
                .iter()
                .filter(|(_, x)| has_annotation(&x.annotations, INJECT))
                .collect()
        };
        for (c, cdef) in chosen {
            for b in params(f, *c, cdef) {
                push(&b.type_name, &b.name, &b.annotations, b.line);
            }
        }
        // Kotlin primary constructor, Java record components.
        if ctors.is_empty() && (f.lang == Lang::Kotlin || def.kind == SymbolKind::Record) {
            for b in f
                .bindings
                .iter()
                .filter(|b| b.scope == Some(d as u32) && b.field)
            {
                push(&b.type_name, &b.name, &b.annotations, b.line);
            }
        }
        // Lombok.
        let lombok_all = def.annotation("AllArgsConstructor").is_some();
        let lombok_required = def.annotation("RequiredArgsConstructor").is_some();
        if ctors.is_empty() && (lombok_all || lombok_required) {
            for (_, x) in members(f, d) {
                if x.kind == SymbolKind::Field
                    && !x.has_modifier("static")
                    && (lombok_all || x.has_modifier("final"))
                    && let Some(t) = &x.type_name
                {
                    push(t, &x.name, &x.annotations, x.line);
                }
            }
        }
    }

    /// Beans injected for `p`: implementations of the injected type that are
    /// beans, `@Bean` producers, Spring Data repositories; narrowed by
    /// qualifier, `@Primary`, then the parameter name.
    fn inject_targets(&self, g: &GraphBuilder, p: &Point) -> Vec<(SymIdx, f32)> {
        let mut cands: Vec<(SymIdx, Bean, f32)> = Vec::new();
        let from_test = g.symbol(p.from).has_role(Role::Test);
        for (ty, c) in g.resolve_type(p.fi, &p.type_name) {
            let sym = g.symbol(ty);
            if sym.has_role(Role::Repository) && sym.kind == SymbolKind::Interface {
                cands.push((
                    ty,
                    Bean {
                        names: vec![decapitalize(&sym.name)],
                        primary: false,
                    },
                    c,
                ));
                continue;
            }
            // The type and its transitive subtypes.
            let mut family = vec![ty];
            let mut i = 0;
            while i < family.len() && family.len() < 64 {
                for &(s, _) in g.subtypes(family[i]) {
                    if !family.contains(&s) {
                        family.push(s);
                    }
                }
                i += 1;
            }
            let mut found = false;
            for &t in &family {
                let s = g.symbol(t);
                if s.has_role(Role::Test) && !from_test {
                    continue;
                }
                if let Some(b) = self.beans.get(&t) {
                    cands.push((t, b.clone(), c));
                    found = true;
                }
                for (m, b) in self.producers.get(&s.name).into_iter().flatten() {
                    cands.push((*m, b.clone(), c));
                    found = true;
                }
            }
            if !found {
                // Concrete classes not declared as beans here (XML, imports...).
                for &t in &family {
                    let s = g.symbol(t);
                    if matches!(
                        s.kind,
                        SymbolKind::Class | SymbolKind::Record | SymbolKind::Object
                    ) && !s.has_role(Role::Test)
                        && !is_abstract(g, self.index, t)
                    {
                        cands.push((
                            t,
                            Bean {
                                names: vec![decapitalize(&s.name)],
                                primary: false,
                            },
                            0.6 * c,
                        ));
                    }
                }
            }
        }
        cands.dedup_by_key(|x| x.0);
        if cands.len() > 1 {
            let narrowed: Vec<_> = match &p.qualifier {
                Some(q) => cands
                    .iter()
                    .filter(|x| x.1.names.contains(q))
                    .cloned()
                    .collect(),
                None => {
                    let primary: Vec<_> = cands.iter().filter(|x| x.1.primary).cloned().collect();
                    if primary.len() == 1 {
                        primary
                    } else {
                        cands
                            .iter()
                            .filter(|x| x.1.names.contains(&p.name))
                            .cloned()
                            .collect()
                    }
                }
            };
            if !narrowed.is_empty() {
                cands = narrowed;
            }
        }
        if cands.len() > crate::graph::resolve::MAX_CANDIDATES {
            return Vec::new();
        }
        let k = cands.len() as f32;
        cands
            .into_iter()
            .map(|(t, _, c)| (t, INJECT_ONE * c / k))
            .collect()
    }

    // ---- configuration

    fn configuration(&mut self, g: &mut GraphBuilder) {
        if self.configs.is_empty() {
            return;
        }
        // (consumer, file, keys or prefix, is prefix, line)
        let mut uses: Vec<(SymIdx, usize, String, bool)> = Vec::new();
        for &fi in &self.files {
            let f = self.file(fi);
            for (d, def) in f.defs.iter().enumerate() {
                let s = g.def_symbol(fi, d);
                for a in &def.annotations {
                    match a.name.as_str() {
                        "Value" => {
                            for k in a.args.iter().flat_map(|x| placeholders(&x.value)) {
                                uses.push((s, fi, k, false));
                            }
                        }
                        "ConfigurationProperties" => {
                            let prefix = a
                                .arg("prefix")
                                .or_else(|| a.value())
                                .and_then(AnnotationArg::as_str)
                                .unwrap_or("");
                            g.add_role(s, Role::Config);
                            add_tag(g, s, "config.prefix", prefix);
                            uses.push((s, fi, prefix.to_string(), true));
                        }
                        "FeignClient" | "ConditionalOnProperty" => {
                            for k in a.args.iter().flat_map(|x| placeholders(&x.value)) {
                                uses.push((s, fi, k, false));
                            }
                        }
                        _ => {}
                    }
                }
            }
            for b in &f.bindings {
                for a in b.annotations.iter().filter(|a| a.name == "Value") {
                    let scope = match b.scope {
                        Some(sc) if f.defs[sc as usize].kind == SymbolKind::Constructor => {
                            f.defs[sc as usize].parent
                        }
                        sc => sc,
                    };
                    let s = g.scope_symbol(fi, scope);
                    for k in a.args.iter().flat_map(|x| placeholders(&x.value)) {
                        uses.push((s, fi, k, false));
                    }
                }
            }
        }
        for (s, fi, key, prefix) in uses {
            add_tag(g, s, "config.keys", &key);
            let root = module_root(&self.file(fi).path);
            let rk = relaxed_key(&key);
            let mut same: Vec<(usize, u32)> = Vec::new();
            let mut other: Vec<(usize, u32)> = Vec::new();
            for c in &self.configs {
                let hit = c.keys.iter().find(|(k, ..)| {
                    if prefix {
                        rk.is_empty() || k.starts_with(&format!("{rk}."))
                    } else {
                        *k == rk
                    }
                });
                if let Some((_, _, _, line)) = hit {
                    if c.root == root
                        || root.starts_with(&format!("{}/", c.root))
                        || c.root.is_empty()
                    {
                        same.push((c.file, *line));
                    } else {
                        other.push((c.file, *line));
                    }
                }
            }
            let (hits, conf) = if same.is_empty() {
                (other, CONFIG_OTHER_MODULE)
            } else {
                (same, CONFIG_SAME_MODULE)
            };
            for (cf, line) in hits {
                let from = g.file_symbol(cf);
                g.add_edge(from, s, EdgeKind::Configures, conf, line);
            }
        }
    }

    // ---- events

    fn events(&mut self, g: &mut GraphBuilder) {
        if self.listeners.is_empty() {
            return;
        }
        // Resolved listener types.
        let listeners: Vec<(SymIdx, Vec<String>, Vec<SymIdx>)> = self
            .listeners
            .iter()
            .map(|(m, fi, types)| {
                let syms = types
                    .iter()
                    .flat_map(|t| g.resolve_type(*fi, t))
                    .map(|(s, _)| s)
                    .collect();
                (*m, types.clone(), syms)
            })
            .collect();
        let mut edges = Vec::new();
        for &fi in &self.files {
            let f = self.file(fi);
            for r in &f.refs {
                if r.kind != RefKind::Call
                    || !matches!(r.name.as_str(), "publishEvent" | "multicastEvent")
                {
                    continue;
                }
                let Some(arg) = r.arg.as_deref() else {
                    continue;
                };
                let Some(event) = event_type(f, r.scope, r.line, arg) else {
                    continue;
                };
                let from = g.scope_symbol(fi, r.scope);
                edges.push((fi, from, event, r.line));
            }
        }
        for (fi, from, event, line) in edges {
            add_tag(g, from, "event.publishes", &event);
            // The event type and its supertypes.
            let mut family: Vec<SymIdx> = g
                .resolve_type(fi, &event)
                .into_iter()
                .map(|(s, _)| s)
                .collect();
            let mut i = 0;
            while i < family.len() && family.len() < 32 {
                for &(s, _) in g.supertypes(family[i]) {
                    if !family.contains(&s) {
                        family.push(s);
                    }
                }
                i += 1;
            }
            let names: HashSet<&str> = family.iter().map(|&s| g.symbol(s).name.as_str()).collect();
            let mut found = Vec::new();
            for (m, types, syms) in &listeners {
                if syms.iter().any(|s| family.contains(s)) {
                    found.push((*m, 0.85));
                } else if syms.is_empty()
                    && types
                        .iter()
                        .any(|t| *t == event || names.contains(t.as_str()))
                {
                    found.push((*m, 0.6));
                }
            }
            for (m, c) in found {
                g.add_edge(from, m, EdgeKind::Publishes, c, line);
            }
        }
    }

    // ---- HTTP clients

    fn http_clients(&mut self, g: &mut GraphBuilder) {
        for &fi in &self.files {
            let f = self.file(fi);
            for b in &f.bindings {
                if let Some(tag) = client_kind(&b.type_name, &b.name) {
                    let owner = super::util::enclosing_type(f, b.scope);
                    let s = g.scope_symbol(fi, owner.map(|d| d as u32));
                    add_tag(g, s, "external", tag);
                    if b.field {
                        g.add_role(s, Role::External);
                    }
                }
            }
            for r in f.refs.iter().filter(|r| r.kind == RefKind::Call) {
                let Some(recv) = r.receiver.as_deref() else {
                    continue;
                };
                let root = recv.split(['.', '(']).next().unwrap_or(recv);
                let tag = if let Some((_, t)) = HTTP_CLIENTS.iter().find(|(t, _)| *t == root) {
                    Some(*t)
                } else {
                    receiver_name(root).and_then(|(name, this)| {
                        let b = binding_at(f, r.scope, name, r.line, this)?;
                        client_kind(&b.type_name, &b.name)
                    })
                };
                if let Some(t) = tag {
                    let s = g.scope_symbol(fi, r.scope);
                    add_tag(g, s, "external", t);
                }
            }
        }
    }
}

/// `external` tag of a binding typed as an HTTP client (`WebClient.Builder`
/// is seen as `Builder`: the name tells).
pub fn client_kind(type_name: &str, name: &str) -> Option<&'static str> {
    if let Some((_, t)) = HTTP_CLIENTS.iter().find(|(t, _)| *t == type_name) {
        return Some(t);
    }
    let n = name.to_ascii_lowercase();
    (type_name == "Builder")
        .then(|| {
            HTTP_CLIENTS
                .iter()
                .find(|(t, _)| n.contains(&t.to_ascii_lowercase()))
        })
        .flatten()
        .map(|(_, k)| *k)
}

/// An injection point.
struct Point {
    /// Consumer: the class (constructor, field, setter) or the `@Bean` method.
    from: SymIdx,
    fi: usize,
    type_name: String,
    qualifier: Option<String>,
    /// Parameter or field name (Spring's last-resort bean name match).
    name: String,
    line: u32,
}

fn qualifier(annotations: &[Annotation]) -> Option<String> {
    annotations.iter().find_map(|a| match a.name.as_str() {
        "Qualifier" | "Named" => a
            .value()
            .and_then(AnnotationArg::as_str)
            .map(str::to_string),
        "Resource" => a
            .arg("name")
            .and_then(AnnotationArg::as_str)
            .map(str::to_string),
        _ => None,
    })
}

/// Parameter bindings of callable `m`: its first `params` bindings.
fn params<'f>(
    f: &'f FileIndex,
    m: usize,
    def: &Def,
) -> impl Iterator<Item = &'f crate::index::Binding> {
    let n = def.params.unwrap_or(0) as usize;
    f.bindings
        .iter()
        .filter(move |b| b.scope == Some(m as u32) && !b.field)
        .take(n)
}

fn is_abstract(g: &GraphBuilder, index: &Index, t: SymIdx) -> bool {
    let s = g.symbol(t);
    let Some(fi) = g.file_index(&s.file) else {
        return false;
    };
    let f = &index.files[fi];
    (0..f.defs.len())
        .find(|&d| g.def_symbol(fi, d) == t)
        .is_some_and(|d| f.defs[d].has_modifier("abstract"))
}

/// Type of the event in `publishEvent(arg)`: `new X(...)`, `X(...)`, or a
/// typed name.
fn event_type(f: &FileIndex, scope: Option<u32>, line: u32, arg: &str) -> Option<String> {
    let a = arg.trim();
    let a = a.strip_prefix("new ").unwrap_or(a).trim();
    let head: String = a
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '.')
        .collect();
    let simple = head.rsplit('.').next().unwrap_or(&head).to_string();
    if a[head.len()..].trim_start().starts_with(['(', '<', '{'])
        && simple.starts_with(|c: char| c.is_ascii_uppercase())
    {
        return Some(simple);
    }
    let (name, this) = receiver_name(a)?;
    binding_at(f, scope, name, line, this).map(|b| b.type_name.clone())
}

/// Strings of an annotation value, constants resolved: `"/a"`,
/// `{"/a", "/b"}`, `["/a"]`, `arrayOf("/a")`, `BASE + "/x"`.
fn annotation_strings(g: &GraphBuilder, fi: usize, value: &str) -> Vec<String> {
    let v = value.trim();
    let inner = v
        .strip_prefix("arrayOf(")
        .and_then(|x| x.strip_suffix(')'))
        .or_else(|| v.strip_prefix('{').and_then(|x| x.strip_suffix('}')))
        .or_else(|| v.strip_prefix('[').and_then(|x| x.strip_suffix(']')))
        .unwrap_or(v);
    let mut out: Vec<String> = split_top(inner, ',')
        .into_iter()
        .map(str::trim)
        .filter(|x| !x.is_empty())
        .map(|x| match unquote(x) {
            Some(s) if !s.contains('$') => s.to_string(),
            _ => parts_to_path(&eval(g, fi, None, 0, x)),
        })
        .collect();
    if out.is_empty() {
        out.push(String::new());
    }
    out
}

/// Parts of an evaluated path, unknown parts as `*`.
pub fn parts_to_path(parts: &[Part]) -> String {
    parts
        .iter()
        .map(|p| match p {
            Part::Lit(s) => s.as_str(),
            Part::Wild => "*",
        })
        .collect()
}

/// `/api` + `owners/{id}/` → `/api/owners/{id}`.
pub fn join_path(prefix: &str, path: &str) -> String {
    let segs: Vec<&str> = prefix
        .split('/')
        .chain(path.split('/'))
        .filter(|s| !s.is_empty())
        .collect();
    format!("/{}", segs.join("/"))
}
