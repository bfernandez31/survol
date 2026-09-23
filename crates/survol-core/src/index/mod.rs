//! Tree-sitter index of a revision: definitions, references, imports and typed
//! bindings of every Java, Kotlin, TypeScript and JavaScript file, plus the
//! keys of Spring / OpenAPI configuration files and the custom elements of
//! HTML templates (read without tree-sitter, see `resource.rs`).
//!
//! Files come from the git tree of the revision (no checkout needed) and are
//! parsed in parallel. Each file's result is cached by blob id under
//! `.git/survol/cache/index/`, so a reopen or a new version of the merge
//! request only parses the blobs that changed.
//!
//! What is extracted is driven by one query per language (`queries/*.scm`,
//! tags.scm style); annotations, containers, arity and imports are then read
//! from the syntax tree in [`extract`]. The index is purely syntactic: turning
//! references into edges is the job of [`crate::graph`].

mod cache;
mod extract;
mod lang;
mod resource;
#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use globset::GlobSet;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

pub use extract::parse_file;
pub use lang::{Lang, is_api_spec_name, is_config_name};
pub use resource::html_elements;

use crate::git::{Git, TreeEntry};

/// Version of the queries and of the extraction code: part of the cache path.
/// Bump it whenever `queries/*.scm` or `extract.rs` change what is produced.
pub const INDEX_VERSION: u32 = 2;

/// Files bigger than this are not parsed (minified bundles, data dumps...).
pub const MAX_FILE_BYTES: u64 = 512 * 1024;

/// Paths never indexed, on top of the mechanical globs.
pub const SKIP_GLOBS: &[&str] = &[
    "**/node_modules/**",
    "**/vendor/**",
    "**/dist/**",
    "**/build/**",
    "**/target/**",
    "**/out/**",
    "**/.gradle/**",
    "**/*.d.ts",
    "**/*.bundle.js",
    "**/*.chunk.js",
];

/// Kind of a definition, and of a graph symbol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SymbolKind {
    /// A whole source file (graph only): owns imports and top-level code.
    File,
    Class,
    Interface,
    Enum,
    Record,
    /// Kotlin `object` / `companion object`.
    Object,
    /// Annotation type declaration (`@interface`, `annotation class`).
    Annotation,
    Method,
    Function,
    Constructor,
    /// Field, property or enum constant.
    Field,
}

impl SymbolKind {
    /// Class-like: can contain members and be extended or instantiated.
    pub fn is_type(self) -> bool {
        matches!(
            self,
            Self::Class
                | Self::Interface
                | Self::Enum
                | Self::Record
                | Self::Object
                | Self::Annotation
        )
    }

    pub fn is_callable(self) -> bool {
        matches!(self, Self::Method | Self::Function | Self::Constructor)
    }
}

/// Lines of a node, 1-based and inclusive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Span {
    pub start: u32,
    pub end: u32,
}

impl Span {
    pub fn contains(&self, line: u32) -> bool {
        self.start <= line && line <= self.end
    }

    /// Number of lines.
    pub fn lines(&self) -> u32 {
        self.end + 1 - self.start
    }
}

/// An annotation (Java, Kotlin) or decorator (TypeScript, JavaScript).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Annotation {
    /// Simple name, without `@` nor qualifier: `GetMapping`, `Component`.
    pub name: String,
    /// Arguments in source order. A single object literal argument
    /// (`@Component({selector: 'x'})`) is flattened into its keyed pairs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<AnnotationArg>,
    pub line: u32,
}

impl Annotation {
    /// Argument named `key`.
    pub fn arg(&self, key: &str) -> Option<&AnnotationArg> {
        self.args.iter().find(|a| a.key.as_deref() == Some(key))
    }

    /// The main value: first positional argument, else the `value` argument
    /// (Java's implicit element name).
    pub fn value(&self) -> Option<&AnnotationArg> {
        self.args
            .iter()
            .find(|a| a.key.is_none())
            .or_else(|| self.arg("value"))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnnotationArg {
    /// `None` for a positional argument.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// Raw source text of the value: `"/owners/{id}"`, `RequestMethod.GET`,
    /// `{"/a", "/b"}`, `['./x.css']`...
    pub value: String,
}

impl AnnotationArg {
    /// The value without quotes when it is a single string literal.
    pub fn as_str(&self) -> Option<&str> {
        unquote(&self.value)
    }

    /// Every string literal in the value: `{"/a", "/b"}` → `["/a", "/b"]`.
    pub fn strings(&self) -> Vec<&str> {
        string_literals(&self.value)
    }
}

/// `"x"`, `'x'` or `` `x` `` → `x`.
pub fn unquote(s: &str) -> Option<&str> {
    let s = s.trim();
    let q = s.chars().next()?;
    (s.len() >= 2 && matches!(q, '"' | '\'' | '`') && s.ends_with(q)).then(|| &s[1..s.len() - 1])
}

pub(crate) fn string_literals(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = s;
    while let Some(start) = rest.find(['"', '\'', '`']) {
        let q = rest[start..].chars().next().expect("found");
        let body = &rest[start + 1..];
        let Some(end) = body.find(q) else { break };
        out.push(&body[..end]);
        rest = &body[end + 1..];
    }
    out
}

/// A definition found in a file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Def {
    pub name: String,
    pub kind: SymbolKind,
    /// Whole node, including annotations and body.
    pub span: Span,
    /// Line of the name, where an editor should jump.
    pub line: u32,
    /// Enclosing definition, index into [`FileIndex::defs`].
    pub parent: Option<u32>,
    /// Annotations and decorators, in source order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub annotations: Vec<Annotation>,
    /// Number of declared parameters (callables, and Kotlin/record primary
    /// constructors on the type itself).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<u16>,
    /// Last parameter is variadic (`...args`, `vararg`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub variadic: bool,
    /// Declared type of a field, return type of a callable: simple name,
    /// generics stripped (`Owner`, `Observable`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub type_name: Option<String>,
    /// Modifiers useful to framework rules, among [`MODIFIERS`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub modifiers: Vec<String>,
}

/// Modifiers kept in [`Def::modifiers`].
pub const MODIFIERS: &[&str] = &[
    "static", "final", "abstract", "readonly", "val", "var", "lateinit", "const", "open",
];

impl Def {
    pub fn has_modifier(&self, m: &str) -> bool {
        self.modifiers.iter().any(|x| x == m)
    }

    pub fn annotation(&self, name: &str) -> Option<&Annotation> {
        self.annotations.iter().find(|a| a.name == name)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefKind {
    /// Function or method call, method reference (`Foo::bar`, `::bar`).
    Call,
    /// `new Foo(...)` (Kotlin constructor calls are plain calls).
    New,
    /// Type used in a signature, a field, a variable, a generic...
    Type,
    Extends,
    Implements,
}

/// A reference to something by name, not resolved yet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ref {
    pub kind: RefKind,
    /// Simple name: `findById`, `OwnerRepository`.
    pub name: String,
    /// Receiver expression as written (`this.owners`, `repo`, `Owner`),
    /// truncated; `None` for an unqualified call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receiver: Option<String>,
    /// Number of arguments of a call, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arity: Option<u16>,
    pub line: u32,
    /// Innermost enclosing definition, index into [`FileIndex::defs`].
    pub scope: Option<u32>,
    /// Source text of the first argument of a call or instantiation, when
    /// it is a literal, a name, a member access, a concatenation, a template,
    /// a `new` or a class literal (URLs, events, injection tokens...).
    /// Whitespace collapsed, truncated to [`MAX_ARG`] characters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arg: Option<String>,
}

/// Longest [`Ref::arg`] and [`Value::text`] kept (object literals: [`MAX_OBJECT_VALUE`]).
pub const MAX_ARG: usize = 200;

/// Longest object literal kept as a [`Value`] (`environment.ts`...).
pub const MAX_OBJECT_VALUE: usize = 2000;

/// An import (Java, Kotlin), ES import / re-export or `require`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Import {
    /// Package (`org.acme.owner`, or the class for a static import) or
    /// module specifier (`./owner.service`, `@angular/core`).
    pub module: String,
    /// Imported names; empty for a wildcard or a bare `require`/side-effect import.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub names: Vec<ImportedName>,
    /// `import a.b.*`, `import * as ns from`, `require(...)`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub wildcard: bool,
    /// Local name of a namespace import (`* as ns`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alias: Option<String>,
    /// Java `import static`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_static: bool,
    pub line: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportedName {
    /// Exported name (`default` for an ES default import).
    pub name: String,
    /// Local name, when renamed (`as`), or for a default import.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alias: Option<String>,
}

impl ImportedName {
    /// Name used in the importing file.
    pub fn local(&self) -> &str {
        self.alias.as_deref().unwrap_or(&self.name)
    }
}

/// A name with a declared (or trivially inferred) type: field, parameter,
/// local variable, constructor property. Used to resolve call receivers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Binding {
    pub name: String,
    /// Simple type name, generics stripped: `OwnerRepository`.
    pub type_name: String,
    /// Definition the name is visible in: the method for a parameter or a
    /// local, the class for a field or a constructor property.
    pub scope: Option<u32>,
    /// A member of the class (field, constructor property), not a local.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub field: bool,
    pub line: u32,
    /// Annotations or decorators of a parameter (`@Qualifier("x")`,
    /// `@Value("${a.b}")`, `@Inject(TOKEN)`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub annotations: Vec<Annotation>,
}

/// A name initialised with a string-like expression: constant, field or
/// local (`entityUrl = environment.API + 'owners'`), or a small object
/// literal (`environment = {...}`). Lets rules rebuild URLs and keys.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Value {
    pub name: String,
    /// Source text of the initialiser, whitespace collapsed.
    pub text: String,
    /// Innermost enclosing definition (the class for a field, the
    /// callable for a local), `None` at top level.
    pub scope: Option<u32>,
    pub line: u32,
}

/// An object literal shaped like a route (`{path: 'x', component: X}`,
/// with `component`, `loadComponent`, `loadChildren`, `children` or
/// `redirectTo`): Angular `Routes`, `provideRouter`, `RouterModule.forRoot`...
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouteDef {
    /// `path` as written, without quotes (may be empty).
    pub path: String,
    /// `component: X` → `X`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub component: Option<String>,
    /// `loadComponent` / `loadChildren: () => import('./x').then(m => m.X)`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub load: Option<LazyLoad>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub redirect: Option<String>,
    /// Enclosing route (through `children`), index into [`FileIndex::routes`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<u32>,
    pub line: u32,
    /// Innermost enclosing definition.
    pub scope: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LazyLoad {
    /// `loadChildren` (a module or routes) rather than `loadComponent`.
    pub children: bool,
    /// Module specifier of the dynamic import: `./owners/owners.module`.
    pub module: String,
    /// Member picked in `then(m => m.X)`; `None` for a default export.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub export: Option<String>,
}

/// A key of a configuration file (`application*.yml|properties`, OpenAPI
/// spec), flattened: `spring.datasource.url`, `paths./owners.get.operationId`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigEntry {
    pub key: String,
    /// Scalar value, unquoted, truncated to [`MAX_ARG`] characters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    pub line: u32,
}

/// A custom element used in an HTML template (`<app-owner-list>`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Element {
    pub name: String,
    pub line: u32,
}

/// Everything extracted from one file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileIndex {
    pub path: String,
    pub lang: Lang,
    /// Java / Kotlin package.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package: Option<String>,
    pub defs: Vec<Def>,
    pub refs: Vec<Ref>,
    pub imports: Vec<Import>,
    pub bindings: Vec<Binding>,
    /// String-like initialisers of constants, fields and locals.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub values: Vec<Value>,
    /// Route-shaped object literals (TypeScript / JavaScript).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub routes: Vec<RouteDef>,
    /// Keys of a configuration file ([`Lang::Yaml`], [`Lang::Properties`]).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub config: Vec<ConfigEntry>,
    /// Custom elements of an HTML template ([`Lang::Html`]).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub elements: Vec<Element>,
    /// Lines in the file.
    pub lines: u32,
    /// The parser hit syntax errors: the result may be partial.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub has_errors: bool,
}

impl FileIndex {
    /// Innermost definition containing `line`.
    pub fn def_at(&self, line: u32) -> Option<usize> {
        self.defs
            .iter()
            .enumerate()
            .filter(|(_, d)| d.span.contains(line))
            .min_by_key(|(_, d)| d.span.lines())
            .map(|(i, _)| i)
    }

    /// `Outer.Inner.method`: names of `def` and its enclosing definitions.
    pub fn qualified_name(&self, def: usize) -> String {
        let mut names = vec![self.defs[def].name.as_str()];
        let mut p = self.defs[def].parent;
        while let Some(i) = p {
            names.push(&self.defs[i as usize].name);
            p = self.defs[i as usize].parent;
        }
        names.reverse();
        names.join(".")
    }

    /// Stable ids of the definitions, see [`crate::graph::Symbol::id`]:
    /// `path#Outer.method/2` (`/n`: parameter count of callables), with a
    /// `~k` suffix to tell identical signatures apart.
    pub fn def_ids(&self) -> Vec<String> {
        let mut seen: HashMap<String, u32> = HashMap::new();
        (0..self.defs.len())
            .map(|i| {
                let d = &self.defs[i];
                let mut id = format!("{}#{}", self.path, self.qualified_name(i));
                if d.kind.is_callable() {
                    id.push_str(&format!("/{}", d.params.unwrap_or(0)));
                }
                let n = seen.entry(id.clone()).or_default();
                *n += 1;
                if *n > 1 {
                    id.push_str(&format!("~{n}"));
                }
                id
            })
            .collect()
    }
}

/// Files of the head, and base versions of the files a review changes.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Index {
    /// Head revision, sorted by path.
    pub files: Vec<FileIndex>,
    /// Base revision of the modified, renamed and deleted files, sorted by path.
    pub base_files: Vec<FileIndex>,
    pub stats: IndexStats,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexStats {
    /// Files indexed (head + base).
    pub files: usize,
    /// Distinct blobs parsed now.
    pub parsed: usize,
    /// Distinct blobs loaded from the cache.
    pub cached: usize,
    /// Source files skipped: too big, vendored, generated.
    pub skipped: usize,
    /// Files whose parse had syntax errors.
    pub with_errors: usize,
    pub millis: u64,
}

impl Index {
    /// Adds the files of `other` (another repository: the back end of a
    /// front end...) under `prefix/`, so that one graph links both (HTTP
    /// calls, shared types). Paths of `self` are left as they are.
    pub fn merge(&mut self, other: Index, prefix: &str) {
        let prefix = prefix.trim_end_matches('/');
        let rebase = |mut f: FileIndex| {
            f.path = format!("{prefix}/{}", f.path);
            f
        };
        self.files.extend(other.files.into_iter().map(rebase));
        self.base_files
            .extend(other.base_files.into_iter().map(rebase));
        self.files.sort_by(|a, b| a.path.cmp(&b.path));
        self.base_files.sort_by(|a, b| a.path.cmp(&b.path));
        let (s, o) = (&mut self.stats, other.stats);
        s.files += o.files;
        s.parsed += o.parsed;
        s.cached += o.cached;
        s.skipped += o.skipped;
        s.with_errors += o.with_errors;
        s.millis += o.millis;
    }

    pub fn file(&self, path: &str) -> Option<&FileIndex> {
        find_sorted(&self.files, path)
    }

    pub fn base_file(&self, path: &str) -> Option<&FileIndex> {
        find_sorted(&self.base_files, path)
    }
}

fn find_sorted<'a>(files: &'a [FileIndex], path: &str) -> Option<&'a FileIndex> {
    files
        .binary_search_by(|f| f.path.as_str().cmp(path))
        .ok()
        .map(|i| &files[i])
}

/// What to index.
#[derive(Debug, Clone)]
pub struct Options {
    /// Paths not to index (vendored, generated...).
    pub skip: GlobSet,
    pub max_file_bytes: u64,
    /// Where parsed files are cached, `None` to disable the cache.
    pub cache_dir: Option<PathBuf>,
}

impl Options {
    /// [`SKIP_GLOBS`] + the mechanical globs (built-in and `extra`), and the
    /// cache under `survol_dir`.
    pub fn new(extra_globs: &[String], survol_dir: Option<&Path>) -> Result<Self, globset::Error> {
        let extra: Vec<String> = SKIP_GLOBS
            .iter()
            .map(|g| g.to_string())
            .chain(extra_globs.iter().cloned())
            .collect();
        Ok(Self {
            skip: crate::mechanical::globset(&extra)?,
            max_file_bytes: MAX_FILE_BYTES,
            cache_dir: survol_dir.map(cache::dir),
        })
    }
}

impl Index {
    /// Indexes the tree of `head`, and the files `base_paths` of `base`
    /// (usually the old paths of the modified, renamed and deleted files).
    pub fn build(
        repo: &Git,
        head: &str,
        base: Option<(&str, &[String])>,
        opts: &Options,
        progress: &mut dyn FnMut(&str),
    ) -> crate::Result<Self> {
        let started = Instant::now();
        let mut stats = IndexStats::default();
        let head_entries = select(repo.ls_tree(head)?, opts, &mut stats);
        let base_entries = match base {
            Some((rev, paths)) if !paths.is_empty() => {
                let wanted: std::collections::HashSet<&str> =
                    paths.iter().map(String::as_str).collect();
                let entries = repo
                    .ls_tree(rev)?
                    .into_iter()
                    .filter(|e| wanted.contains(e.path.as_str()))
                    .collect();
                select(entries, opts, &mut IndexStats::default())
            }
            _ => Vec::new(),
        };
        progress(&format!(
            "indexing {} files (+{} base versions)",
            head_entries.len(),
            base_entries.len()
        ));
        let (files, base_files) =
            parse_entries(repo, head_entries, base_entries, opts, &mut stats, progress)?;
        stats.files = files.len() + base_files.len();
        stats.with_errors = files.iter().filter(|f| f.has_errors).count();
        stats.millis = started.elapsed().as_millis() as u64;
        Ok(Self {
            files,
            base_files,
            stats,
        })
    }

    /// Indexes the files of `rev` accepted by `only` (all when `None`) as
    /// the head side of an index, without base versions: the graph of
    /// another revision (the base of a review), from the same blob cache.
    pub fn build_revision(
        repo: &Git,
        rev: &str,
        only: Option<&dyn Fn(&str) -> bool>,
        opts: &Options,
        progress: &mut dyn FnMut(&str),
    ) -> crate::Result<Self> {
        let started = Instant::now();
        let mut stats = IndexStats::default();
        let entries: Vec<TreeEntry> = repo
            .ls_tree(rev)?
            .into_iter()
            .filter(|e| only.is_none_or(|f| f(&e.path)))
            .collect();
        let entries = select(entries, opts, &mut stats);
        progress(&format!(
            "indexing {} files of {}",
            entries.len(),
            &rev[..rev.len().min(8)]
        ));
        let (files, _) = parse_entries(repo, entries, Vec::new(), opts, &mut stats, progress)?;
        stats.files = files.len();
        stats.with_errors = files.iter().filter(|f| f.has_errors).count();
        stats.millis = started.elapsed().as_millis() as u64;
        Ok(Self {
            files,
            base_files: Vec::new(),
            stats,
        })
    }
}

/// Parses (or loads from the cache) the blobs of `head_entries` and
/// `base_entries`, once per distinct blob and language.
fn parse_entries(
    repo: &Git,
    head_entries: Vec<(TreeEntry, Lang)>,
    base_entries: Vec<(TreeEntry, Lang)>,
    opts: &Options,
    stats: &mut IndexStats,
    progress: &mut dyn FnMut(&str),
) -> crate::Result<(Vec<FileIndex>, Vec<FileIndex>)> {
    // One parse per distinct (blob, language), whatever the path or side.
    let mut todo: Vec<(&str, Lang)> = head_entries
        .iter()
        .chain(&base_entries)
        .map(|(e, l)| (e.blob.as_str(), *l))
        .collect();
    todo.sort_unstable();
    todo.dedup();
    let cached: Vec<Option<FileIndex>> = todo
        .par_iter()
        .map(|(blob, lang)| {
            opts.cache_dir
                .as_deref()
                .and_then(|d| cache::load(d, blob, *lang))
        })
        .collect();
    let mut parsed: HashMap<(String, Lang), FileIndex> = HashMap::new();
    let mut missing = Vec::new();
    for ((blob, lang), hit) in todo.into_iter().zip(cached) {
        match hit {
            Some(f) => {
                stats.cached += 1;
                parsed.insert((blob.to_string(), lang), f);
            }
            None => missing.push((blob, lang)),
        }
    }
    if !missing.is_empty() {
        progress(&format!("parsing {} files", missing.len()));
        let ids: Vec<&str> = missing.iter().map(|(b, _)| *b).collect();
        let contents = repo.read_blobs(&ids)?;
        let fresh: Vec<FileIndex> = missing
            .par_iter()
            .zip(contents.par_iter())
            .map(|((blob, lang), src)| {
                let f = parse_file("", *lang, &String::from_utf8_lossy(src));
                if let Some(d) = &opts.cache_dir {
                    // A cache write failure only costs a reparse next time.
                    let _ = cache::save(d, blob, &f);
                }
                f
            })
            .collect();
        stats.parsed = fresh.len();
        for ((blob, lang), f) in missing.into_iter().zip(fresh) {
            parsed.insert((blob.to_string(), lang), f);
        }
    }

    let assemble = |entries: Vec<(TreeEntry, Lang)>| -> Vec<FileIndex> {
        let mut files: Vec<FileIndex> = entries
            .into_iter()
            .filter_map(|(e, lang)| {
                let mut f = parsed.get(&(e.blob, lang))?.clone();
                f.path = e.path;
                Some(f)
            })
            .collect();
        files.sort_by(|a, b| a.path.cmp(&b.path));
        files
    };
    let files = assemble(head_entries);
    let base_files = assemble(base_entries);
    Ok((files, base_files))
}

/// Source files worth indexing, with their language.
fn select(
    entries: Vec<TreeEntry>,
    opts: &Options,
    stats: &mut IndexStats,
) -> Vec<(TreeEntry, Lang)> {
    entries
        .into_iter()
        .filter_map(|e| {
            let lang = Lang::from_path(&e.path)?;
            if e.size > opts.max_file_bytes || opts.skip.is_match(&e.path) {
                stats.skipped += 1;
                return None;
            }
            Some((e, lang))
        })
        .collect()
}
