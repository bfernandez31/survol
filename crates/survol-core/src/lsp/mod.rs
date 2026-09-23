//! Refinement of the code graph by language servers.
//!
//! The tree-sitter graph is heuristic (see [`crate::graph`]). Once it is
//! built, language servers started on the review worktree check the edges
//! that matter for the review: those touching changed callables. Confirmed
//! edges get confidence 1 and [`Edge::lsp`](crate::graph::Edge::lsp);
//! contradicted ones are removed; callers only the server knows are added.
//! See [`refine`] for the rules.
//!
//! Bounded: one hard time budget for the whole run (server start and
//! indexing included), a timeout per request. A missing or failing server
//! only means fewer refined edges: the heuristic graph stays usable.

mod client;
pub mod refine;
mod server;
#[cfg(test)]
mod tests;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub use client::{Client, Notification};
pub use refine::{Backend, Changes, Task, TaskKind, Texts};
pub use server::{Readiness, Server};

use crate::config::{LspConfig, LspServerConfig, expand_home};
use crate::graph::{Graph, GraphData};
use crate::index::Lang;

/// Version of the refinement: part of the cache key.
pub const LSP_VERSION: u32 = 1;

#[derive(Debug, thiserror::Error)]
pub enum LspError {
    #[error("i/o: {0}")]
    Io(std::io::Error),
    #[error("{0}: no answer in time")]
    Timeout(String),
    #[error("server closed the connection")]
    Closed,
    #[error("server error {code}: {message}")]
    Rpc { code: i64, message: String },
}

/// A location returned by a server.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Loc {
    /// Relative to the workspace root; `None` outside of it (a library, a
    /// `jdt://` or `jar:` URI).
    pub path: Option<String>,
    /// 1-based.
    pub line: u32,
    /// UTF-16.
    pub column: u32,
}

/// Language families, one server each.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Family {
    Java,
    Kotlin,
    /// TypeScript and JavaScript.
    Typescript,
}

impl Family {
    pub const ALL: [Family; 3] = [Family::Java, Family::Kotlin, Family::Typescript];

    pub fn of(lang: Lang) -> Option<Self> {
        match lang {
            Lang::Java => Some(Family::Java),
            Lang::Kotlin => Some(Family::Kotlin),
            Lang::TypeScript | Lang::Tsx | Lang::JavaScript => Some(Family::Typescript),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Family::Java => "java",
            Family::Kotlin => "kotlin",
            Family::Typescript => "typescript",
        }
    }

    fn config(self, cfg: &LspConfig) -> &LspServerConfig {
        match self {
            Family::Java => &cfg.java,
            Family::Kotlin => &cfg.kotlin,
            Family::Typescript => &cfg.typescript,
        }
    }
}

/// A built-in server: command and default arguments.
#[derive(Debug, Clone, Copy)]
pub struct Candidate {
    pub command: &'static str,
    pub args: &'static [&'static str],
    /// How to install it, for `doctor`.
    pub install: &'static str,
}

/// Built-in servers of a family, preferred first.
pub fn candidates(family: Family) -> &'static [Candidate] {
    match family {
        Family::Java => &[Candidate {
            command: "jdtls",
            args: &["-data", "{data}"],
            install: "brew install jdtls",
        }],
        Family::Kotlin => &[
            Candidate {
                command: "kotlin-language-server",
                args: &[],
                install: "brew install kotlin-language-server",
            },
            Candidate {
                command: "kotlin-lsp",
                args: &["--stdio", "--system-path", "{data}"],
                install: "brew install --cask kotlin-lsp",
            },
        ],
        Family::Typescript => &[Candidate {
            command: "typescript-language-server",
            args: &["--stdio"],
            install: "npm i -g typescript-language-server typescript",
        }],
    }
}

/// A server ready to be started.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerSpec {
    pub family: Family,
    /// Command as configured (or the built-in one).
    pub command: String,
    /// Resolved executable.
    pub path: PathBuf,
    /// Arguments, `{data}` still in place.
    pub args: Vec<String>,
    /// Extra environment variables (`~` expanded in values).
    pub env: Vec<(String, String)>,
    pub readiness: Readiness,
}

impl ServerSpec {
    /// Short name: the executable's file name.
    pub fn name(&self) -> String {
        Path::new(&self.command).file_name().map_or_else(
            || self.command.clone(),
            |n| n.to_string_lossy().into_owned(),
        )
    }

    /// The arguments with `{data}` replaced.
    fn args_with(&self, data: &Path) -> Vec<String> {
        let data = data.to_string_lossy();
        self.args
            .iter()
            .map(|a| a.replace("{data}", &data))
            .collect()
    }

    /// `initializationOptions` of the server.
    fn init_options(&self, data: &Path) -> Value {
        match self.name().as_str() {
            "jdtls" => json!({
                "settings": {"java": {
                    "import": {"generatesMetadataFilesAtProjectRoot": false},
                    "autobuild": {"enabled": false},
                }},
                "extendedClientCapabilities": {"progressReportProvider": true},
            }),
            "kotlin-lsp" | "intellij-server" => json!({"indexDir": data}),
            "kotlin-language-server" => json!({"storagePath": data}),
            _ => Value::Null,
        }
    }
}

/// Why a family has no server to start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unavailable {
    Disabled,
    /// The command (or none of the built-in ones) is in `PATH`.
    Missing(String),
}

/// The server to use for `family`: the configured command, else the first
/// built-in one found in `PATH`.
pub fn resolve(cfg: &LspConfig, family: Family) -> Result<ServerSpec, Unavailable> {
    let sc = family.config(cfg);
    if !cfg.enabled || !sc.enabled {
        return Err(Unavailable::Disabled);
    }
    let spec = |command: &str, path: PathBuf, args: Vec<String>| {
        let name = Path::new(command)
            .file_name()
            .map_or(command.to_string(), |n| n.to_string_lossy().into_owned());
        ServerSpec {
            family,
            command: command.to_string(),
            path,
            args,
            env: sc
                .env
                .iter()
                .map(|(k, v)| {
                    (
                        k.clone(),
                        expand_home(Path::new(v)).to_string_lossy().into_owned(),
                    )
                })
                .collect(),
            readiness: match name.as_str() {
                "jdtls" => Readiness::ServiceReady,
                "typescript-language-server" => Readiness::Immediate,
                _ if family == Family::Typescript => Readiness::Immediate,
                _ => Readiness::Progress,
            },
        }
    };
    if let Some(command) = &sc.command {
        let path = which(command).ok_or_else(|| Unavailable::Missing(command.clone()))?;
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned());
        let args = if sc.args.is_empty() {
            candidates(family)
                .iter()
                .find(|c| Some(c.command) == name.as_deref())
                .map(|c| c.args.iter().map(|a| a.to_string()).collect())
                .unwrap_or_default()
        } else {
            sc.args.clone()
        };
        return Ok(spec(command, path, args));
    }
    for c in candidates(family) {
        if let Some(path) = which(c.command) {
            let args = if sc.args.is_empty() {
                c.args.iter().map(|a| a.to_string()).collect()
            } else {
                sc.args.clone()
            };
            return Ok(spec(c.command, path, args));
        }
    }
    let names: Vec<&str> = candidates(family).iter().map(|c| c.command).collect();
    Err(Unavailable::Missing(names.join(" or ")))
}

/// `command` if it is a path to a file, else its first match in `PATH`.
pub fn which(command: &str) -> Option<PathBuf> {
    let expanded = expand_home(Path::new(command));
    if expanded.components().count() > 1 {
        return expanded.is_file().then_some(expanded);
    }
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|d| d.join(command))
        .find(|p| p.is_file())
}

/// How one server did.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ServerReport {
    pub family: Option<Family>,
    /// `jdtls`, `kotlin-lsp`...; empty when missing.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub server: String,
    /// `ready`, `missing`, `disabled`, `not ready` (budget reached while
    /// indexing), `failed`.
    pub status: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub detail: String,
    /// Questions for this server, and how many were answered.
    pub tasks: usize,
    pub answered: usize,
    /// Start and `initialize`, then indexing, in milliseconds.
    pub start_ms: u64,
    pub ready_ms: u64,
}

/// What a refinement did, kept in the refined graph.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LspStats {
    pub servers: Vec<ServerReport>,
    /// Questions planned, answered, failed (timeouts, errors).
    pub tasks: usize,
    pub answered: usize,
    pub failed: usize,
    pub confirmed: usize,
    pub removed: usize,
    pub added: usize,
    pub millis: u64,
    /// Every question was answered or failed within the budget.
    pub complete: bool,
}

impl LspStats {
    /// At least one server answered: worth caching.
    pub fn any_ready(&self) -> bool {
        self.servers.iter().any(|s| s.status == "ready")
    }

    /// One line for a status bar.
    pub fn summary(&self) -> String {
        let servers: Vec<String> = self
            .servers
            .iter()
            .map(|s| match s.server.as_str() {
                "" => format!("{} {}", s.family.map_or("?", Family::name), s.status),
                n if s.status == "ready" => n.to_string(),
                n => format!("{n} {}", s.status),
            })
            .collect();
        format!(
            "LSP: {} confirmed, {} removed, {} added ({}/{} answered; {})",
            self.confirmed,
            self.removed,
            self.added,
            self.answered,
            self.tasks,
            servers.join(", ")
        )
    }
}

/// Where and how to refine.
pub struct Options<'a> {
    /// The review worktree: the servers' workspace.
    pub root: &'a Path,
    /// Per-server data directories and logs (`.git/survol/lsp/`).
    pub data_dir: Option<&'a Path>,
    pub cfg: &'a LspConfig,
}

/// Progress shared by the server threads.
struct Progress<'a> {
    report: &'a (dyn Fn(&str) + Sync),
    state: Mutex<(usize, usize, BTreeMap<String, String>)>,
}

impl Progress<'_> {
    fn update(&self, f: impl FnOnce(&mut (usize, usize, BTreeMap<String, String>))) {
        let msg = {
            let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
            f(&mut st);
            let (done, total, status) = &*st;
            let mut msg = format!("LSP: refining {done}/{total}");
            let waiting: Vec<String> = status
                .iter()
                .filter(|(_, s)| !s.is_empty())
                .map(|(n, s)| format!("{n} {s}"))
                .collect();
            if !waiting.is_empty() {
                msg.push_str(&format!(" · {}", waiting.join(" · ")));
            }
            msg
        };
        (self.report)(&msg);
    }
}

/// Refines `graph` with the servers of `opts`. Never fails: a server that
/// is missing, crashes or runs out of time is reported in the stats and
/// its edges stay as they were.
pub fn refine(
    graph: &Graph,
    opts: &Options,
    progress: &(dyn Fn(&str) + Sync),
) -> (Graph, LspStats) {
    refine_with(graph, opts, progress, &|family, deadline, status| {
        start_server(opts, family, deadline, status)
    })
}

/// A started server, or why there is none.
pub type Started = Result<(Box<dyn Backend + Send>, ServerReport), ServerReport>;

/// Starts the server of a family before a deadline, reporting its status.
pub type StartFn<'a> = dyn Fn(Family, Instant, &mut dyn FnMut(&str)) -> Started + Sync + 'a;

/// [`refine`] with the servers started by `start` (tests use fakes).
pub fn refine_with(
    graph: &Graph,
    opts: &Options,
    progress: &(dyn Fn(&str) + Sync),
    start: &StartFn<'_>,
) -> (Graph, LspStats) {
    let started = Instant::now();
    let deadline = started + Duration::from_secs(opts.cfg.budget_secs);
    let mut texts = Texts::new(opts.root);
    let tasks = refine::plan(graph, &mut texts);
    let mut by_family: BTreeMap<Family, Vec<usize>> = BTreeMap::new();
    for (i, t) in tasks.iter().enumerate() {
        if let Some(f) = Family::of(t.lang) {
            by_family.entry(f).or_default().push(i);
        }
    }
    let progress = Progress {
        report: progress,
        state: Mutex::new((0, tasks.len(), BTreeMap::new())),
    };
    let results: Mutex<Vec<Option<Vec<Loc>>>> = Mutex::new(vec![None; tasks.len()]);
    let failed = Mutex::new(0usize);
    let reports: Vec<ServerReport> = std::thread::scope(|sc| {
        let handles: Vec<_> = by_family
            .iter()
            .map(|(&family, idx)| {
                let (tasks, results, failed, progress) = (&tasks, &results, &failed, &progress);
                sc.spawn(move || {
                    run_family(
                        family, idx, tasks, deadline, start, progress, results, failed,
                    )
                })
            })
            .collect();
        handles
            .into_iter()
            .zip(by_family.keys())
            .map(|(h, &family)| {
                h.join().unwrap_or_else(|_| ServerReport {
                    family: Some(family),
                    status: "failed".into(),
                    detail: "panicked".into(),
                    ..Default::default()
                })
            })
            .collect()
    });
    let results = results.into_inner().unwrap_or_else(|e| e.into_inner());
    let failed = failed.into_inner().unwrap_or_else(|e| e.into_inner());
    let (mut data, changes): (GraphData, Changes) =
        refine::apply(graph, &tasks, &results, &mut texts);
    let answered = results.iter().filter(|r| r.is_some()).count();
    let stats = LspStats {
        complete: answered + failed == tasks.len(),
        servers: reports,
        tasks: tasks.len(),
        answered,
        failed,
        confirmed: changes.confirmed,
        removed: changes.removed,
        added: changes.added,
        millis: started.elapsed().as_millis() as u64,
    };
    data.lsp = Some(stats.clone());
    (Graph::new(data), stats)
}

#[allow(clippy::too_many_arguments)]
fn run_family(
    family: Family,
    idx: &[usize],
    tasks: &[Task],
    deadline: Instant,
    start: &StartFn<'_>,
    progress: &Progress,
    results: &Mutex<Vec<Option<Vec<Loc>>>>,
    failed: &Mutex<usize>,
) -> ServerReport {
    let key = family.name().to_string();
    let mut status = |s: &str| {
        let s = format!("indexing… {}", truncate(s, 60))
            .trim_end()
            .to_string();
        progress.update(|st| {
            st.2.insert(key.clone(), s);
        });
    };
    status("");
    let started = start(family, deadline, &mut status);
    progress.update(|st| {
        st.2.remove(&key);
    });
    let (mut backend, mut report) = match started {
        Ok(ok) => ok,
        Err(mut report) => {
            report.tasks = idx.len();
            return report;
        }
    };
    report.tasks = idx.len();
    for &i in idx {
        if Instant::now() >= deadline {
            break;
        }
        match tasks[i].run(backend.as_mut()) {
            Ok(locs) => {
                results.lock().unwrap_or_else(|e| e.into_inner())[i] = Some(locs);
                report.answered += 1;
            }
            Err(_) => *failed.lock().unwrap_or_else(|e| e.into_inner()) += 1,
        }
        progress.update(|st| st.0 += 1);
    }
    drop(backend);
    report
}

fn truncate(s: &str, n: usize) -> String {
    match s.char_indices().nth(n) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_string(),
    }
}

/// Starts, initializes and waits for the server of `family`.
fn start_server(
    opts: &Options,
    family: Family,
    deadline: Instant,
    status: &mut dyn FnMut(&str),
) -> Started {
    let mut report = ServerReport {
        family: Some(family),
        ..Default::default()
    };
    let spec = match resolve(opts.cfg, family) {
        Ok(s) => s,
        Err(Unavailable::Disabled) => {
            report.status = "disabled".into();
            return Err(report);
        }
        Err(Unavailable::Missing(m)) => {
            report.status = "missing".into();
            report.detail = format!("{m} not found");
            return Err(report);
        }
    };
    let (spec, init_override) = match typescript_fallback(&spec, opts.root) {
        Some(Ok(x)) => x,
        Some(Err(detail)) => {
            report.server = spec.name();
            report.status = "failed".into();
            report.detail = detail;
            return Err(report);
        }
        None => (spec, None),
    };
    report.server = spec.name();
    let fail = |mut report: ServerReport, status: &str, e: String| {
        report.status = status.into();
        report.detail = e;
        Err(report)
    };
    let root_hash = blake3::hash(opts.root.to_string_lossy().as_bytes()).to_hex();
    let stem = format!("{}-{}", spec.name(), &root_hash[..12]);
    let (data, log) = match opts.data_dir {
        Some(d) => {
            let _ = std::fs::create_dir_all(d);
            (d.join(&stem), Some(d.join(format!("{stem}.log"))))
        }
        None => (std::env::temp_dir().join("survol-lsp").join(&stem), None),
    };
    let t0 = Instant::now();
    let mut run = spec.clone();
    run.args = spec.args_with(&data);
    let timeout = Duration::from_secs(opts.cfg.request_timeout_secs.max(1));
    let mut server = match Server::spawn(&run, opts.root, log.as_deref(), timeout) {
        Ok(s) => s,
        Err(e) => return fail(report, "failed", e.to_string()),
    };
    let init = init_override.unwrap_or_else(|| spec.init_options(&data));
    if let Err(e) = server.initialize(init, deadline) {
        let st = if Instant::now() >= deadline {
            "not ready"
        } else {
            "failed"
        };
        return fail(report, st, e.to_string());
    }
    report.start_ms = t0.elapsed().as_millis() as u64;
    let t1 = Instant::now();
    match server.wait_ready(deadline, status) {
        Ok(true) => {}
        Ok(false) => {
            report.ready_ms = t1.elapsed().as_millis() as u64;
            server.shutdown();
            return fail(
                report,
                "not ready",
                "time budget reached while indexing".into(),
            );
        }
        Err(e) => return fail(report, "failed", e.to_string()),
    }
    report.ready_ms = t1.elapsed().as_millis() as u64;
    report.status = "ready".into();
    Ok((Box::new(Stopping(Some(server))), report))
}

/// typescript-language-server drives `tsserver.js` (TypeScript ≤ 6): the
/// workspace's, else the one installed next to the server. Without one,
/// the native server of TypeScript ≥ 7 (`tsc --lsp --stdio`) is used.
/// `None`: not typescript-language-server, nothing to adjust.
#[allow(clippy::type_complexity)]
fn typescript_fallback(
    spec: &ServerSpec,
    root: &Path,
) -> Option<Result<(ServerSpec, Option<Value>), String>> {
    if spec.family != Family::Typescript || spec.name() != "typescript-language-server" {
        return None;
    }
    let local = root.join("node_modules/typescript/lib/tsserver.js");
    if local.is_file() {
        return Some(Ok((spec.clone(), None)));
    }
    // <prefix>/lib/node_modules/typescript-language-server/lib/cli.mjs
    let global = std::fs::canonicalize(&spec.path).ok().and_then(|p| {
        let modules = p.ancestors().nth(3)?;
        Some(modules.join("typescript/lib/tsserver.js")).filter(|t| t.is_file())
    });
    if let Some(t) = global {
        return Some(Ok((spec.clone(), Some(json!({"tsserver": {"path": t}})))));
    }
    let native = which("tsc").filter(|tsc| {
        std::process::Command::new(tsc)
            .arg("--version")
            .output()
            .ok()
            .and_then(|o| {
                let v = String::from_utf8_lossy(&o.stdout).into_owned();
                let major = v
                    .trim()
                    .strip_prefix("Version ")?
                    .split('.')
                    .next()?
                    .parse::<u32>()
                    .ok()?;
                Some(major >= 7)
            })
            .unwrap_or(false)
    });
    Some(match native {
        Some(path) => Ok((
            ServerSpec {
                command: "tsc".into(),
                path,
                args: vec!["--lsp".into(), "--stdio".into()],
                ..spec.clone()
            },
            None,
        )),
        None => Err(
            "no TypeScript found (node_modules/typescript, global typescript@5, or tsc ≥ 7)".into(),
        ),
    })
}

/// A server shut down cleanly when dropped.
struct Stopping(Option<Server>);

impl Backend for Stopping {
    fn definition(&mut self, path: &str, line: u32, column: u32) -> Result<Vec<Loc>, LspError> {
        self.0
            .as_mut()
            .ok_or(LspError::Closed)?
            .definition(path, line, column)
    }

    fn references(&mut self, path: &str, line: u32, column: u32) -> Result<Vec<Loc>, LspError> {
        self.0
            .as_mut()
            .ok_or(LspError::Closed)?
            .references(path, line, column)
    }
}

impl Drop for Stopping {
    fn drop(&mut self) {
        if let Some(s) = self.0.take() {
            s.shutdown();
        }
    }
}

/// Key of a cached refinement: the graph's key, the refinement version and
/// the servers that would run (command, arguments).
pub fn cache_key(graph_key: &str, cfg: &LspConfig) -> String {
    let mut h = blake3::Hasher::new();
    h.update(format!("lsp{LSP_VERSION}\0{graph_key}\0").as_bytes());
    for f in Family::ALL {
        match resolve(cfg, f) {
            Ok(s) => {
                h.update(s.path.to_string_lossy().as_bytes());
                for a in s.args.iter().chain(s.env.iter().map(|(_, v)| v)) {
                    h.update(b"\0");
                    h.update(a.as_bytes());
                }
            }
            Err(e) => {
                h.update(format!("{e:?}").as_bytes());
            }
        }
        h.update(b"\n");
    }
    h.finalize().to_hex()[..16].to_string()
}

/// `.git/survol/cache/<head_sha>/graph-lsp.json`
pub fn cache_path(survol_dir: &Path, head_sha: &str) -> PathBuf {
    survol_dir
        .join("cache")
        .join(head_sha)
        .join("graph-lsp.json")
}

/// Status of each family's server, for `doctor`.
pub fn detect(cfg: &LspConfig) -> Vec<(Family, Result<ServerSpec, Unavailable>)> {
    Family::ALL.iter().map(|&f| (f, resolve(cfg, f))).collect()
}
