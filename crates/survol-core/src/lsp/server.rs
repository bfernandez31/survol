//! One language server: started on the review worktree, initialized, waited
//! for until it has indexed the project, then asked for definitions and
//! references.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::client::{Client, Notification};
use super::{Loc, LspError, ServerSpec};
use crate::index::Lang;

/// How to tell that a server has finished indexing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Readiness {
    /// Answers as soon as initialized (typescript-language-server: the
    /// project is loaded on the first opened file).
    Immediate,
    /// jdtls: `language/status` `ServiceReady`.
    ServiceReady,
    /// Work done progress: ready once every `$/progress` has ended and the
    /// server has been quiet for a moment (kotlin servers). Also ready on
    /// `intellij/ready-for-test` (JetBrains kotlin-lsp).
    Progress,
}

/// A running server.
pub struct Server {
    client: Client,
    child: Option<Child>,
    root: PathBuf,
    /// The root as the server may report it (symlinks resolved).
    canonical_root: PathBuf,
    opened: HashSet<String>,
    readiness: Readiness,
    timeout: Duration,
    /// Text of the files opened, for `didOpen`.
    texts: HashMap<String, String>,
}

impl Server {
    /// Starts `spec` with `root` as workspace, its stderr going to `log`.
    pub fn spawn(
        spec: &ServerSpec,
        root: &Path,
        log: Option<&Path>,
        timeout: Duration,
    ) -> Result<Self, LspError> {
        let stderr = match log.and_then(|p| std::fs::File::create(p).ok()) {
            Some(f) => Stdio::from(f),
            None => Stdio::null(),
        };
        let mut child = Command::new(&spec.path)
            .args(&spec.args)
            .envs(spec.env.iter().map(|(k, v)| (k, v)))
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(stderr)
            .spawn()
            .map_err(LspError::Io)?;
        let stdin = child.stdin.take().ok_or(LspError::Closed)?;
        let stdout = child.stdout.take().ok_or(LspError::Closed)?;
        let mut s = Self::with_client(Client::new(stdout, stdin), root, spec.readiness, timeout);
        s.child = Some(child);
        Ok(s)
    }

    /// A server behind an existing connection (tests: in-process fakes).
    pub fn with_client(
        client: Client,
        root: &Path,
        readiness: Readiness,
        timeout: Duration,
    ) -> Self {
        Self {
            client,
            child: None,
            root: root.to_path_buf(),
            canonical_root: root.canonicalize().unwrap_or_else(|_| root.to_path_buf()),
            opened: HashSet::new(),
            readiness,
            timeout,
            texts: HashMap::new(),
        }
    }

    /// `initialize` then `initialized`. The start of a JVM server can be
    /// slow: waits until `deadline`.
    pub fn initialize(&mut self, options: Value, deadline: Instant) -> Result<(), LspError> {
        let uri = file_uri(&self.root);
        let name = self
            .root
            .file_name()
            .map_or("root".into(), |n| n.to_string_lossy().into_owned());
        let params = json!({
            "processId": std::process::id(),
            "clientInfo": {"name": "survol", "version": env!("CARGO_PKG_VERSION")},
            "rootUri": uri,
            "rootPath": self.root,
            "workspaceFolders": [{"uri": uri, "name": name}],
            "capabilities": {
                "window": {"workDoneProgress": true},
                "workspace": {"workspaceFolders": true, "configuration": true},
                "textDocument": {
                    "synchronization": {"didSave": false},
                    "definition": {"linkSupport": false},
                    "references": {},
                },
            },
            "initializationOptions": options,
        });
        self.client
            .request("initialize", params, remaining(deadline))?;
        self.client.notify("initialized", json!({}))
    }

    /// Waits until the server has indexed the project, reporting its
    /// status messages. `Ok(false)`: `deadline` reached first.
    pub fn wait_ready(
        &mut self,
        deadline: Instant,
        status: &mut dyn FnMut(&str),
    ) -> Result<bool, LspError> {
        if self.readiness == Readiness::Immediate {
            return Ok(true);
        }
        let started = Instant::now();
        let mut active: HashSet<String> = HashSet::new();
        let mut last_event = Instant::now();
        let mut seen_progress = false;
        loop {
            let now = Instant::now();
            if now >= deadline {
                return Ok(false);
            }
            if self.readiness == Readiness::Progress
                && active.is_empty()
                && now.duration_since(last_event) >= QUIET
                // Servers may start reporting progress a little after
                // `initialized`: give them a moment.
                && (seen_progress || now.duration_since(started) >= FIRST_PROGRESS)
            {
                return Ok(true);
            }
            let wait = (deadline - now).min(Duration::from_millis(200));
            let Some(n) = self.client.next_notification(wait)? else {
                continue;
            };
            match readiness_event(&n) {
                Event::Ready => return Ok(true),
                Event::Begin(token, msg) => {
                    seen_progress = true;
                    last_event = Instant::now();
                    active.insert(token);
                    status(&msg);
                }
                Event::Report(msg) => {
                    last_event = Instant::now();
                    status(&msg);
                }
                Event::End(token) => {
                    last_event = Instant::now();
                    active.remove(&token);
                }
                Event::Status(msg) => status(&msg),
                Event::Other => {}
            }
        }
    }

    /// Where the symbol used at `path:line:column` is defined (1-based line,
    /// UTF-16 column).
    pub fn definition(&mut self, path: &str, line: u32, column: u32) -> Result<Vec<Loc>, LspError> {
        self.open(path)?;
        let res = self.client.request(
            "textDocument/definition",
            self.position(path, line, column),
            self.timeout,
        )?;
        Ok(self.locations(&res))
    }

    /// Where the symbol declared at `path:line:column` is used, declaration
    /// excluded.
    pub fn references(&mut self, path: &str, line: u32, column: u32) -> Result<Vec<Loc>, LspError> {
        self.open(path)?;
        let mut params = self.position(path, line, column);
        params["context"] = json!({"includeDeclaration": false});
        let res = self
            .client
            .request("textDocument/references", params, self.timeout)?;
        Ok(self.locations(&res))
    }

    /// Asks the server to stop, then kills it if it does not.
    pub fn shutdown(mut self) {
        let _ = self
            .client
            .request("shutdown", Value::Null, Duration::from_secs(2));
        let _ = self.client.notify("exit", Value::Null);
        if let Some(mut child) = self.child.take() {
            let until = Instant::now() + Duration::from_secs(2);
            while Instant::now() < until {
                if let Ok(Some(_)) = child.try_wait() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    /// Sets the text sent on `didOpen` for `path` (else read from disk).
    pub fn set_text(&mut self, path: &str, text: String) {
        self.texts.insert(path.to_string(), text);
    }

    fn open(&mut self, path: &str) -> Result<(), LspError> {
        if !self.opened.insert(path.to_string()) {
            return Ok(());
        }
        let text = match self.texts.get(path) {
            Some(t) => t.clone(),
            None => std::fs::read_to_string(self.root.join(path)).unwrap_or_default(),
        };
        self.client.notify(
            "textDocument/didOpen",
            json!({"textDocument": {
                "uri": file_uri(&self.root.join(path)),
                "languageId": language_id(path),
                "version": 1,
                "text": text,
            }}),
        )
    }

    fn position(&self, path: &str, line: u32, column: u32) -> Value {
        json!({
            "textDocument": {"uri": file_uri(&self.root.join(path))},
            "position": {"line": line.saturating_sub(1), "character": column},
        })
    }

    /// `Location | Location[] | LocationLink[] | null` → locations, with
    /// paths relative to the root (`None`: outside of it, a library).
    fn locations(&self, v: &Value) -> Vec<Loc> {
        let items: Vec<&Value> = match v {
            Value::Array(a) => a.iter().collect(),
            Value::Object(_) => vec![v],
            _ => Vec::new(),
        };
        items
            .into_iter()
            .filter_map(|l| {
                let uri = l
                    .get("uri")
                    .or_else(|| l.get("targetUri"))
                    .and_then(Value::as_str)?;
                let range = l.get("targetSelectionRange").or_else(|| l.get("range"))?;
                let start = range.get("start")?;
                Some(Loc {
                    path: self.relative(uri),
                    line: start.get("line")?.as_u64()? as u32 + 1,
                    column: start.get("character")?.as_u64()? as u32,
                })
            })
            .collect()
    }

    fn relative(&self, uri: &str) -> Option<String> {
        let path = url::Url::parse(uri).ok()?.to_file_path().ok()?;
        let rel = path
            .strip_prefix(&self.root)
            .or_else(|_| path.strip_prefix(&self.canonical_root))
            .ok()?;
        Some(rel.to_string_lossy().replace('\\', "/"))
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if let Some(child) = &mut self.child {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Quiet period after the last progress event before a [`Readiness::Progress`]
/// server is considered ready.
const QUIET: Duration = Duration::from_millis(1500);
/// How long to wait for a first progress report.
const FIRST_PROGRESS: Duration = Duration::from_secs(4);

enum Event {
    Ready,
    Begin(String, String),
    Report(String),
    End(String),
    Status(String),
    Other,
}

fn readiness_event(n: &Notification) -> Event {
    let p = &n.params;
    let s = |v: Option<&Value>| v.and_then(Value::as_str).unwrap_or("").to_string();
    match n.method.as_str() {
        "language/status" => {
            if s(p.get("type")) == "ServiceReady" {
                Event::Ready
            } else {
                Event::Status(s(p.get("message")))
            }
        }
        "intellij/ready-for-test" => Event::Ready,
        "$/progress" => {
            let token = match p.get("token") {
                Some(Value::String(t)) => t.clone(),
                Some(t) => t.to_string(),
                None => String::new(),
            };
            let v = p.get("value").cloned().unwrap_or(Value::Null);
            let mut msg = s(v.get("title"));
            let detail = s(v.get("message"));
            if !detail.is_empty() {
                if !msg.is_empty() {
                    msg.push_str(": ");
                }
                msg.push_str(&detail);
            }
            if let Some(pct) = v.get("percentage").and_then(Value::as_u64) {
                msg.push_str(&format!(" {pct}%"));
            }
            match v.get("kind").and_then(Value::as_str) {
                Some("begin") => Event::Begin(token, msg),
                Some("report") => Event::Report(msg),
                Some("end") => Event::End(token),
                _ => Event::Other,
            }
        }
        _ => Event::Other,
    }
}

fn remaining(deadline: Instant) -> Duration {
    deadline
        .saturating_duration_since(Instant::now())
        .max(Duration::from_millis(1))
}

pub fn file_uri(path: &Path) -> String {
    url::Url::from_file_path(path)
        .map_or_else(|_| format!("file://{}", path.display()), |u| u.to_string())
}

/// LSP language id of a file.
pub fn language_id(path: &str) -> &'static str {
    match Lang::from_path(path) {
        Some(Lang::Java) => "java",
        Some(Lang::Kotlin) => "kotlin",
        Some(Lang::Tsx) => "typescriptreact",
        Some(Lang::TypeScript) => "typescript",
        _ if path.ends_with(".jsx") => "javascriptreact",
        _ => "javascript",
    }
}
