//! Heuristic resolution of references to symbols.
//!
//! Types are looked up in the same file, then explicit imports, the same
//! package, wildcard imports, and last globally by name. Calls use the
//! receiver: `this`/none → the enclosing class hierarchy, a typed name
//! (field, parameter, local) → that type's members, a capitalised name → a
//! static member, anything else → methods of that name anywhere, preferring
//! types visible from the file. Arity then narrows overloads.
//!
//! Confidence policy: 0.9–1.0 when the target is determined (same file,
//! import, typed receiver); ~0.85 same package; 0.5 when only the name and
//! visibility agree; 0.3 for a global guess; divided by the number of
//! candidates when several remain. More than [`MAX_CANDIDATES`] → no edge.
//! Imports of external packages end resolution with no edge, so a library
//! type never matches a same-named project type.

use std::collections::{HashMap, HashSet};

use super::build::GraphBuilder;
use super::{SymIdx, SymbolKind};
use crate::index::{FileIndex, Lang, Ref, RefKind};

/// More candidates than this: the reference is too ambiguous to give edges.
pub const MAX_CANDIDATES: usize = 5;

/// Unqualified method names too common to guess from on an unknown receiver
/// (collections, streams, promises, observables, builders...).
const COMMON_NAMES: &[&str] = &[
    "get",
    "set",
    "add",
    "put",
    "remove",
    "delete",
    "clear",
    "contains",
    "size",
    "length",
    "isEmpty",
    "equals",
    "hashCode",
    "toString",
    "compareTo",
    "valueOf",
    "of",
    "from",
    "map",
    "flatMap",
    "filter",
    "reduce",
    "forEach",
    "collect",
    "stream",
    "toList",
    "sort",
    "find",
    "findFirst",
    "first",
    "last",
    "orElse",
    "orElseThrow",
    "ifPresent",
    "then",
    "catch",
    "finally",
    "subscribe",
    "pipe",
    "next",
    "error",
    "complete",
    "emit",
    "push",
    "pop",
    "join",
    "split",
    "trim",
    "replace",
    "format",
    "append",
    "build",
    "builder",
    "apply",
    "let",
    "run",
    "also",
    "use",
    "close",
    "open",
    "start",
    "stop",
    "init",
    "update",
    "create",
    "save",
    "load",
    "read",
    "write",
    "send",
    "call",
    "invoke",
    "execute",
    "handle",
    "process",
    "log",
    "debug",
    "info",
    "warn",
    "print",
    "println",
    "assertThat",
    "assertEquals",
    "expect",
    "toBe",
    "toEqual",
    "when",
    "verify",
    "mock",
    "spyOn",
    "describe",
    "it",
    "test",
    "beforeEach",
];

/// Marker: too many candidates.
pub struct Ambiguous;

type Cands = Vec<(SymIdx, f32)>;

/// Lookup tables over the head symbols.
#[derive(Default)]
pub struct Tables {
    pub by_name: HashMap<String, Vec<SymIdx>>,
    /// `package.Outer.Inner.member` → symbols (Java/Kotlin).
    by_qname: HashMap<String, Vec<SymIdx>>,
    /// (type, member name) → members, companion members included.
    members: HashMap<(SymIdx, String), Vec<SymIdx>>,
    /// Type → its members (same content as `members`).
    members_by_type: HashMap<SymIdx, Vec<SymIdx>>,
    pub supers: HashMap<SymIdx, Cands>,
    pub subs: HashMap<SymIdx, Cands>,
    /// Types with a supertype that is not in the index (a library class).
    pub external_super: HashSet<SymIdx>,
}

impl Tables {
    pub fn new(b: &GraphBuilder) -> Self {
        let mut t = Tables::default();
        for (fi, f) in b.index.files.iter().enumerate() {
            for (d, _) in f.defs.iter().enumerate() {
                let s = b.def_syms[fi][d];
                let sym = &b.symbols[s as usize];
                t.by_name.entry(sym.name.clone()).or_default().push(s);
                if let Some(p) = &f.package {
                    t.by_qname
                        .entry(format!("{p}.{}", f.qualified_name(d)))
                        .or_default()
                        .push(s);
                }
                if let Some(c) = sym.container {
                    let cont = &b.symbols[c as usize];
                    if cont.kind.is_type() {
                        t.members.entry((c, sym.name.clone())).or_default().push(s);
                        t.members_by_type.entry(c).or_default().push(s);
                        // Companion members are reachable as `Outer.member`.
                        if cont.kind == SymbolKind::Object
                            && cont.name == "Companion"
                            && let Some(outer) = cont.container
                        {
                            t.members
                                .entry((outer, sym.name.clone()))
                                .or_default()
                                .push(s);
                            t.members_by_type.entry(outer).or_default().push(s);
                        }
                    }
                }
            }
        }
        t
    }

    /// Members of `ty`.
    pub fn members_of(&self, ty: SymIdx) -> &[SymIdx] {
        self.members_by_type.get(&ty).map_or(&[], Vec::as_slice)
    }

    /// Members named `name` of `ty`, else of the nearest supertypes having one
    /// (breadth first).
    pub fn members_in_hierarchy(&self, ty: SymIdx, name: &str) -> Vec<SymIdx> {
        let mut level = vec![ty];
        let mut seen = HashSet::from([ty]);
        while !level.is_empty() {
            let found: Vec<SymIdx> = level
                .iter()
                .filter_map(|&t| self.members.get(&(t, name.to_string())))
                .flatten()
                .copied()
                .collect();
            if !found.is_empty() {
                return found;
            }
            level = level
                .iter()
                .filter_map(|t| self.supers.get(t))
                .flatten()
                .map(|&(s, _)| s)
                .filter(|s| seen.insert(*s))
                .collect();
        }
        Vec::new()
    }

    fn qname(&self, b: &GraphBuilder, q: &str, pred: impl Fn(SymbolKind) -> bool) -> Vec<SymIdx> {
        self.by_qname
            .get(q)
            .into_iter()
            .flatten()
            .copied()
            .filter(|&s| pred(b.symbols[s as usize].kind))
            .collect()
    }

    pub fn resolve_type(&self, b: &GraphBuilder, fi: usize, name: &str) -> Cands {
        let f = &b.index.files[fi];
        let is_type = |s: SymIdx| b.symbols[s as usize].kind.is_type();
        let same_file: Vec<SymIdx> = f
            .defs
            .iter()
            .enumerate()
            .filter(|(_, d)| d.name == name && d.kind.is_type())
            .map(|(d, _)| b.def_syms[fi][d])
            .collect();
        if !same_file.is_empty() {
            return split(same_file, 1.0);
        }
        if f.lang.is_jvm() {
            for imp in f.imports.iter().filter(|i| !i.wildcard && !i.is_static) {
                if let Some(n) = imp.names.iter().find(|n| n.local() == name) {
                    // Explicit import: the type is this one or external.
                    let found = self.qname(
                        b,
                        &format!("{}.{}", imp.module, n.name),
                        SymbolKind::is_type,
                    );
                    return split(found, 0.95);
                }
            }
            if let Some(p) = &f.package {
                let found = self.qname(b, &format!("{p}.{name}"), SymbolKind::is_type);
                if !found.is_empty() {
                    return split(found, 0.9);
                }
            }
            let found: Vec<SymIdx> = f
                .imports
                .iter()
                .filter(|i| i.wildcard)
                .flat_map(|i| self.qname(b, &format!("{}.{name}", i.module), SymbolKind::is_type))
                .collect();
            if !found.is_empty() {
                return split(found, 0.85);
            }
        } else if let Some(found) = self.imported_from_module(b, fi, name, is_type) {
            return split(found, 0.95);
        }
        let global: Vec<SymIdx> = self
            .by_name
            .get(name)
            .into_iter()
            .flatten()
            .copied()
            .filter(|&s| is_type(s) && visible_from(b, fi, s))
            .collect();
        split(global, 0.3)
    }

    /// TS/JS: `name` imported in file `fi` → the matching top-level symbols
    /// of the imported file (`Some(empty)` when imported from a package).
    fn imported_from_module(
        &self,
        b: &GraphBuilder,
        fi: usize,
        name: &str,
        pred: impl Fn(SymIdx) -> bool,
    ) -> Option<Vec<SymIdx>> {
        let f = &b.index.files[fi];
        for imp in &f.imports {
            if let Some(n) = imp.names.iter().find(|n| n.local() == name) {
                let Some(target) = resolve_module(b, &f.path, &imp.module) else {
                    return Some(Vec::new());
                };
                let tf = &b.index.files[target];
                let exported = if n.name == "default" {
                    None
                } else {
                    Some(n.name.as_str())
                };
                let found: Vec<SymIdx> = tf
                    .defs
                    .iter()
                    .enumerate()
                    .filter(|(_, d)| d.parent.is_none() && exported.is_none_or(|e| d.name == e))
                    .map(|(d, _)| b.def_syms[target][d])
                    .filter(|&s| pred(s))
                    .collect();
                return Some(found);
            }
        }
        None
    }

    /// Top-level functions named `name` visible from file `fi` without a receiver.
    fn resolve_function(&self, b: &GraphBuilder, fi: usize, name: &str) -> Cands {
        let f = &b.index.files[fi];
        let top_level = |s: SymIdx| {
            let sym = &b.symbols[s as usize];
            sym.kind.is_callable()
                && sym
                    .container
                    .is_none_or(|c| b.symbols[c as usize].kind == SymbolKind::File)
        };
        let same_file: Vec<SymIdx> = b.def_syms[fi]
            .iter()
            .copied()
            .filter(|&s| top_level(s) && b.symbols[s as usize].name == name)
            .collect();
        if !same_file.is_empty() {
            return split(same_file, 0.95);
        }
        if f.lang.is_jvm() {
            let callable = |k: SymbolKind| k.is_callable();
            for imp in f.imports.iter().filter(|i| !i.wildcard) {
                if let Some(n) = imp.names.iter().find(|n| n.local() == name) {
                    let found = self.qname(b, &format!("{}.{}", imp.module, n.name), callable);
                    return split(found, 0.9);
                }
            }
            if let Some(p) = &f.package {
                let found = self.qname(b, &format!("{p}.{name}"), callable);
                if !found.is_empty() {
                    return split(found, 0.85);
                }
            }
            let found: Vec<SymIdx> = f
                .imports
                .iter()
                .filter(|i| i.wildcard)
                .flat_map(|i| self.qname(b, &format!("{}.{name}", i.module), callable))
                .collect();
            if !found.is_empty() {
                return split(found, 0.8);
            }
            if f.lang == Lang::Java {
                return Vec::new();
            }
        } else if let Some(found) = self.imported_from_module(b, fi, name, top_level) {
            return split(found, 0.95);
        }
        let global: Vec<SymIdx> = self
            .by_name
            .get(name)
            .into_iter()
            .flatten()
            .copied()
            .filter(|&s| top_level(s) && visible_from(b, fi, s))
            .collect();
        split(global, 0.3)
    }

    /// Targets of a call, instantiation or type reference.
    pub fn resolve_ref(&self, b: &GraphBuilder, fi: usize, r: &Ref) -> Result<Cands, Ambiguous> {
        let cands = match r.kind {
            RefKind::Type => self.resolve_type(b, fi, &r.name),
            RefKind::New => self.constructors(b, fi, &r.name, r.arity),
            RefKind::Call => self.resolve_call(b, fi, r),
            RefKind::Extends | RefKind::Implements => Vec::new(),
        };
        if cands.len() > MAX_CANDIDATES {
            return Err(Ambiguous);
        }
        Ok(cands)
    }

    /// `new T(...)` / Kotlin `T(...)`: matching constructors, else the type.
    fn constructors(&self, b: &GraphBuilder, fi: usize, name: &str, arity: Option<u16>) -> Cands {
        let mut out = Vec::new();
        for (t, c) in self.resolve_type(b, fi, name) {
            let ctors: Vec<SymIdx> = self
                .members
                .get(&(t, b.symbols[t as usize].name.clone()))
                .into_iter()
                .flatten()
                .copied()
                .filter(|&s| b.symbols[s as usize].kind == SymbolKind::Constructor)
                .collect();
            let mut options: Cands = ctors.iter().map(|&s| (s, c)).collect();
            // Primary constructor (Kotlin, records, TS without constructor).
            options.push((t, c));
            let matched = by_arity(b, options, arity, b.index.files[fi].lang);
            // The type itself only when no explicit constructor matches.
            let has_ctor = matched.iter().any(|&(s, _)| s != t);
            out.extend(matched.into_iter().filter(|&(s, _)| !has_ctor || s != t));
        }
        out
    }

    fn resolve_call(&self, b: &GraphBuilder, fi: usize, r: &Ref) -> Cands {
        let f = &b.index.files[fi];
        let lang = f.lang;
        let enclosing = enclosing_type(b, fi, r.scope);
        let receiver = r.receiver.as_deref().map(|s| {
            s.strip_prefix("this.")
                .or_else(|| s.strip_prefix("this?."))
                .map_or((s, false), |rest| (rest, true))
        });
        let cands = match receiver {
            None => {
                // Own class, then outer classes.
                let mut t = enclosing;
                let mut found = Vec::new();
                while let Some(ty) = t {
                    let ms = self.callable_members(b, ty, &r.name);
                    if !ms.is_empty() {
                        found = split(ms, 0.9);
                        break;
                    }
                    t = outer_type(b, ty);
                }
                if found.is_empty() {
                    found = self.resolve_function(b, fi, &r.name);
                }
                if found.is_empty() && r.name.starts_with(|c: char| c.is_ascii_uppercase()) {
                    return self.constructors(b, fi, &r.name, r.arity);
                }
                found
            }
            Some(("this" | "self", _)) => enclosing
                .map(|t| split(self.callable_members(b, t, &r.name), 0.9))
                .unwrap_or_default(),
            Some(("super", _)) => enclosing
                .and_then(|t| self.supers.get(&t))
                .into_iter()
                .flatten()
                .flat_map(|&(s, c)| split(self.callable_members(b, s, &r.name), 0.9 * c))
                .collect(),
            Some((recv, via_this)) if is_identifier(recv) => {
                match self.binding_type(b, fi, r, recv, via_this, enclosing) {
                    Some(ty) => self.on_type(b, fi, &ty, &r.name, 0.95),
                    None if recv.starts_with(|c: char| c.is_ascii_uppercase()) => {
                        self.on_type(b, fi, recv, &r.name, 0.95)
                    }
                    None => match self.namespace_import(b, fi, recv) {
                        Some(target) => split(
                            b.def_syms[target]
                                .iter()
                                .copied()
                                .filter(|&s| {
                                    let sym = &b.symbols[s as usize];
                                    sym.name == r.name
                                        && b.symbols[sym.container.unwrap_or(s) as usize].kind
                                            == SymbolKind::File
                                })
                                .collect(),
                            0.9,
                        ),
                        None => self.unknown_receiver(b, fi, r),
                    },
                }
            }
            Some(_) => self.unknown_receiver(b, fi, r),
        };
        by_arity(b, cands, r.arity, lang)
    }

    fn callable_members(&self, b: &GraphBuilder, ty: SymIdx, name: &str) -> Vec<SymIdx> {
        self.members_in_hierarchy(ty, name)
            .into_iter()
            .filter(|&s| b.symbols[s as usize].kind.is_callable())
            .collect()
    }

    /// Members `name` of the type named `ty` as seen from file `fi`. No
    /// edge when the type is not in the index (a library type), or when the
    /// member is not found (inherited from a library type, or dynamic).
    fn on_type(&self, b: &GraphBuilder, fi: usize, ty: &str, name: &str, factor: f32) -> Cands {
        let mut out = Vec::new();
        let types = self.resolve_type(b, fi, ty);
        for (t, c) in types {
            out.extend(split(self.callable_members(b, t, name), c * factor));
        }
        out
    }

    /// Declared type of `name` at the reference: a parameter or local of an
    /// enclosing callable, else a field of the enclosing types.
    fn binding_type(
        &self,
        b: &GraphBuilder,
        fi: usize,
        r: &Ref,
        name: &str,
        fields_only: bool,
        enclosing: Option<SymIdx>,
    ) -> Option<String> {
        let f = &b.index.files[fi];
        let mut scope = r.scope;
        while let Some(s) = scope {
            let d = &f.defs[s as usize];
            let is_type = d.kind.is_type();
            if !fields_only || is_type {
                let found = f.bindings.iter().rfind(|x| {
                    x.scope == Some(s)
                        && x.name == name
                        && (!fields_only || x.field)
                        && (is_type || x.line <= r.line)
                });
                if let Some(x) = found {
                    return Some(x.type_name.clone());
                }
            }
            scope = d.parent;
        }
        // Inherited field: a binding in a supertype declared in another file.
        let t = enclosing?;
        let mut level: Vec<SymIdx> = self.supers.get(&t)?.iter().map(|&(s, _)| s).collect();
        let mut seen = HashSet::from([t]);
        while let Some(s) = level.pop() {
            if !seen.insert(s) {
                continue;
            }
            let file = b.file_index(&b.symbols[s as usize].file)?;
            let def = b.def_syms[file].iter().position(|&x| x == s)? as u32;
            if let Some(x) = b.index.files[file]
                .bindings
                .iter()
                .find(|x| x.scope == Some(def) && x.field && x.name == name)
            {
                return Some(x.type_name.clone());
            }
            level.extend(self.supers.get(&s).into_iter().flatten().map(|&(p, _)| p));
        }
        None
    }

    /// TS/JS `import * as ns from './x'`: `ns` → the file.
    fn namespace_import(&self, b: &GraphBuilder, fi: usize, name: &str) -> Option<usize> {
        let f = &b.index.files[fi];
        let imp = f
            .imports
            .iter()
            .find(|i| i.wildcard && i.alias.as_deref() == Some(name))?;
        resolve_module(b, &f.path, &imp.module)
    }

    /// Receiver of unknown type (call chain, untyped variable, lambda
    /// parameter): methods of that name, preferring types visible from the
    /// file. Never for common library names.
    fn unknown_receiver(&self, b: &GraphBuilder, fi: usize, r: &Ref) -> Cands {
        if COMMON_NAMES.contains(&r.name.as_str()) || r.name.len() < 3 {
            return Vec::new();
        }
        let methods: Vec<SymIdx> = self
            .by_name
            .get(&r.name)
            .into_iter()
            .flatten()
            .copied()
            .filter(|&s| {
                let sym = &b.symbols[s as usize];
                sym.kind == SymbolKind::Method && visible_from(b, fi, s)
            })
            .collect();
        let methods = by_arity(
            b,
            methods.into_iter().map(|s| (s, 1.0)).collect(),
            r.arity,
            b.index.files[fi].lang,
        );
        let visible: Vec<SymIdx> = methods
            .iter()
            .map(|&(s, _)| s)
            .filter(|&s| {
                let ty = b.symbols[s as usize]
                    .container
                    .expect("methods have a container");
                self.type_visible(b, fi, ty)
            })
            .collect();
        if !visible.is_empty() {
            return split(visible, 0.5);
        }
        if methods.len() <= 2 {
            return split(methods.into_iter().map(|(s, _)| s).collect(), 0.3);
        }
        Vec::new()
    }

    /// Type `ty` is declared in, imported by or in the same package as file `fi`.
    fn type_visible(&self, b: &GraphBuilder, fi: usize, ty: SymIdx) -> bool {
        let sym = &b.symbols[ty as usize];
        let f = &b.index.files[fi];
        if sym.file == f.path {
            return true;
        }
        let Some(tf) = b.file_index(&sym.file) else {
            return false;
        };
        let tfile = &b.index.files[tf];
        if f.lang.is_jvm() {
            if tfile.package.is_some() && tfile.package == f.package {
                return true;
            }
            f.imports.iter().any(|i| {
                Some(&i.module) == tfile.package.as_ref()
                    && (i.wildcard || i.names.iter().any(|n| n.name == sym.name))
            })
        } else {
            f.imports
                .iter()
                .any(|i| resolve_module(b, &f.path, &i.module) == Some(tf))
        }
    }

    /// Import edges of file `fi`: (target, confidence, line).
    pub fn import_targets(&self, b: &GraphBuilder, fi: usize) -> Vec<(SymIdx, f32, u32)> {
        let f = &b.index.files[fi];
        let mut out = Vec::new();
        for imp in &f.imports {
            if f.lang.is_jvm() {
                for n in &imp.names {
                    let found = self.qname(b, &format!("{}.{}", imp.module, n.name), |_| true);
                    out.extend(split(found, 1.0).into_iter().map(|(s, c)| (s, c, imp.line)));
                }
            } else if let Some(target) = resolve_module(b, &f.path, &imp.module) {
                let named: Vec<SymIdx> = imp
                    .names
                    .iter()
                    .filter(|n| n.name != "default")
                    .flat_map(|n| {
                        b.def_syms[target].iter().copied().filter(|&s| {
                            let sym = &b.symbols[s as usize];
                            sym.name == n.name
                                && sym
                                    .container
                                    .is_none_or(|c| b.symbols[c as usize].kind == SymbolKind::File)
                        })
                    })
                    .collect();
                if named.is_empty() {
                    out.push((b.file_syms[target], 1.0, imp.line));
                } else {
                    out.extend(named.into_iter().map(|s| (s, 1.0, imp.line)));
                }
            }
        }
        out
    }
}

/// Main code never calls test code: test symbols are only visible from tests.
fn visible_from(b: &GraphBuilder, fi: usize, s: SymIdx) -> bool {
    !b.symbols[s as usize].has_role(super::Role::Test) || b.files[fi].test
}

/// Keeps the candidates whose parameter count fits `arity`. Exact matches
/// (or variadic ones) win; with none, Kotlin/TS/JS keep callables with more
/// parameters (default values, confidence × 0.8), Java keeps none.
/// Candidates without a known count always stay.
fn by_arity(b: &GraphBuilder, cands: Cands, arity: Option<u16>, lang: Lang) -> Cands {
    let Some(a) = arity else { return cands };
    let fits = |s: SymIdx| {
        let sym = &b.symbols[s as usize];
        match sym.params {
            None => true,
            Some(p) => p == a || (sym.variadic && a + 1 >= p),
        }
    };
    let exact: Cands = cands.iter().copied().filter(|&(s, _)| fits(s)).collect();
    if !exact.is_empty() || lang == Lang::Java {
        return renormalise(exact, cands.len());
    }
    let loose: Cands = cands
        .iter()
        .copied()
        .filter(|&(s, _)| b.symbols[s as usize].params.is_some_and(|p| p > a))
        .map(|(s, c)| (s, c * 0.8))
        .collect();
    renormalise(loose, cands.len())
}

/// After filtering `before` candidates down to `after.len()`, give back the
/// confidence lost to the eliminated ones.
fn renormalise(after: Cands, before: usize) -> Cands {
    let k = after.len();
    if k == 0 || k == before {
        return after;
    }
    let factor = before as f32 / k as f32;
    after
        .into_iter()
        .map(|(s, c)| (s, (c * factor).min(1.0)))
        .collect()
}

/// Spreads `confidence` over the candidates.
fn split(cands: Vec<SymIdx>, confidence: f32) -> Cands {
    let k = cands.len().max(1) as f32;
    cands.into_iter().map(|s| (s, confidence / k)).collect()
}

fn is_identifier(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '$')
        && !s.starts_with(|c: char| c.is_ascii_digit())
}

/// Nearest type around a scope (the scope itself when it is a type).
fn enclosing_type(b: &GraphBuilder, fi: usize, scope: Option<u32>) -> Option<SymIdx> {
    let f: &FileIndex = &b.index.files[fi];
    let mut s = scope;
    while let Some(d) = s {
        if f.defs[d as usize].kind.is_type() {
            return Some(b.def_syms[fi][d as usize]);
        }
        s = f.defs[d as usize].parent;
    }
    None
}

fn outer_type(b: &GraphBuilder, ty: SymIdx) -> Option<SymIdx> {
    let mut c = b.symbols[ty as usize].container;
    while let Some(s) = c {
        if b.symbols[s as usize].kind.is_type() {
            return Some(s);
        }
        c = b.symbols[s as usize].container;
    }
    None
}

/// Head file imported by `spec` from `from`: relative specifiers only, with
/// the usual extensions and `index` files.
pub(super) fn resolve_module(b: &GraphBuilder, from: &str, spec: &str) -> Option<usize> {
    if !spec.starts_with('.') {
        return None;
    }
    let dir = from.rsplit_once('/').map_or("", |(d, _)| d);
    let mut parts: Vec<&str> = if dir.is_empty() {
        Vec::new()
    } else {
        dir.split('/').collect()
    };
    for seg in spec.split('/') {
        match seg {
            "." | "" => {}
            ".." => {
                parts.pop()?;
            }
            s => parts.push(s),
        }
    }
    let path = parts.join("/");
    // ESM imports name the compiled file: `./x.js` for `x.ts`.
    let stem = path.strip_suffix(".js").unwrap_or(&path);
    let suffixes = [
        "",
        ".ts",
        ".tsx",
        ".js",
        ".jsx",
        ".mjs",
        ".cjs",
        "/index.ts",
        "/index.tsx",
        "/index.js",
    ];
    [path.as_str(), stem]
        .iter()
        .flat_map(|base| suffixes.iter().map(move |s| format!("{base}{s}")))
        .find_map(|p| b.file_index(&p))
}
