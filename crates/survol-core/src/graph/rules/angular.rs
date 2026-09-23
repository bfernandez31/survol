//! Angular (TypeScript): components, modules, templates, routes, injection
//! and environments.
//!
//! - `@Component` → [`Role::View`] (tags `angular.selector`,
//!   `angular.template`), `@Directive` / `@Pipe` / `@Injectable` →
//!   [`Role::Component`] (`angular.provided_in`), `@NgModule` →
//!   [`Role::Config`]; `uses` edges from a module to its declarations,
//!   imports, providers and bootstrap, and from a standalone component to
//!   its `imports`.
//! - Templates (`templateUrl` files and inline `template`): custom elements
//!   matched to component selectors → `routes` edges, from the HTML file and
//!   from the owning component.
//! - Routes (`Routes` arrays, `provideRouter`, `RouterModule.forRoot/forChild`,
//!   any route-shaped object literal): routed components get
//!   [`Role::EntryPoint`] (`entry = route`, `angular.route` = full path,
//!   parent and lazy-loading prefixes included); `routes` edges from the
//!   route table to the component, or to the lazily loaded module / routes.
//! - Injection (`injects` edges): constructor parameters (`@Inject(TOKEN)`
//!   included) and `inject(X)`, to the class, the `useClass` /
//!   `useExisting` provider, or the `InjectionToken` constant.
//! - `environment*.ts` → [`Role::Config`], `configures` edges to the code
//!   reading `environment.*`.

use std::collections::HashMap;

use super::FrameworkRule;
use super::util::{add_tag, identifiers, is_identifier, members, split_top};
use crate::graph::{EdgeKind, GraphBuilder, Role, SymIdx, SymbolKind};
use crate::index::{AnnotationArg, FileIndex, Index, Lang, RefKind, unquote};

pub struct Angular;

/// Confidence of a template element matched to one component.
const TEMPLATE: f32 = 0.9;
/// Confidence of a route to a component resolved from the route file.
const ROUTE: f32 = 0.95;
const INJECT: f32 = 0.9;

impl FrameworkRule for Angular {
    fn name(&self) -> &str {
        "angular"
    }

    fn apply(&self, index: &Index, g: &mut GraphBuilder) {
        let files: Vec<usize> = (0..index.files.len())
            .filter(|&fi| {
                matches!(
                    index.files[fi].lang,
                    Lang::TypeScript | Lang::Tsx | Lang::JavaScript
                ) && !g.symbol(g.file_symbol(fi)).has_role(Role::Test)
            })
            .collect();
        let angular = files.iter().any(|&fi| {
            index.files[fi]
                .imports
                .iter()
                .any(|i| i.module.starts_with("@angular/"))
        });
        if !angular {
            return;
        }
        let mut cx = Cx {
            index,
            files,
            selectors: HashMap::new(),
            templates: Vec::new(),
            providers: HashMap::new(),
        };
        cx.decorators(g);
        cx.templates(g);
        cx.routes(g);
        cx.injection(g);
        cx.environments(g);
    }
}

struct Cx<'a> {
    index: &'a Index,
    /// Main TS/JS files.
    files: Vec<usize>,
    /// Element selector → components.
    selectors: HashMap<String, Vec<SymIdx>>,
    /// (component, template file or inline text, decorator line).
    templates: Vec<(SymIdx, Template)>,
    /// Provided token name → (file of the provider list, implementation name).
    providers: HashMap<String, Vec<(usize, String)>>,
}

enum Template {
    File(usize, u32),
    Inline(String, u32),
}

impl<'a> Cx<'a> {
    fn file(&self, fi: usize) -> &'a FileIndex {
        &self.index.files[fi]
    }

    fn decorators(&mut self, g: &mut GraphBuilder) {
        let mut uses: Vec<(SymIdx, usize, String, u32)> = Vec::new();
        for &fi in &self.files.clone() {
            let f = self.file(fi);
            for (d, def) in f.defs.iter().enumerate() {
                if def.kind != SymbolKind::Class {
                    continue;
                }
                let s = g.def_symbol(fi, d);
                for a in &def.annotations {
                    match a.name.as_str() {
                        "Component" | "Directive" => {
                            g.add_role(s, Role::Component);
                            if a.name == "Component" {
                                g.add_role(s, Role::View);
                            }
                            if let Some(sel) = a.arg("selector").and_then(AnnotationArg::as_str) {
                                add_tag(g, s, "angular.selector", sel);
                                for el in element_selectors(sel) {
                                    self.selectors.entry(el).or_default().push(s);
                                }
                            }
                            if let Some(url) = a.arg("templateUrl").and_then(AnnotationArg::as_str)
                            {
                                add_tag(g, s, "angular.template", url);
                                if let Some(t) = g.resolve_module(fi, url) {
                                    self.templates.push((s, Template::File(t, a.line)));
                                    let ts = g.file_symbol(t);
                                    g.add_edge(s, ts, EdgeKind::Uses, 0.95, a.line);
                                }
                            }
                            if let Some(t) = a.arg("template") {
                                self.templates
                                    .push((s, Template::Inline(t.value.clone(), a.line)));
                            }
                            if let Some(i) = a.arg("imports") {
                                for n in identifiers(&i.value) {
                                    uses.push((s, fi, n.to_string(), a.line));
                                }
                            }
                            self.collect_providers(fi, a.arg("providers"));
                        }
                        "Pipe" => {
                            g.add_role(s, Role::Component);
                            if let Some(n) = a.arg("name").and_then(AnnotationArg::as_str) {
                                add_tag(g, s, "angular.pipe", n);
                            }
                        }
                        "Injectable" => {
                            g.add_role(s, Role::Component);
                            let p = a
                                .arg("providedIn")
                                .map(|p| p.as_str().unwrap_or(&p.value).to_string());
                            add_tag(
                                g,
                                s,
                                "angular.provided_in",
                                p.as_deref().unwrap_or("module"),
                            );
                        }
                        "NgModule" => {
                            g.add_role(s, Role::Config);
                            for key in [
                                "declarations",
                                "imports",
                                "providers",
                                "bootstrap",
                                "exports",
                            ] {
                                if let Some(v) = a.arg(key) {
                                    for n in identifiers(&v.value) {
                                        if n != "RouterModule" && n != "BrowserModule" {
                                            uses.push((s, fi, n.to_string(), a.line));
                                        }
                                    }
                                }
                            }
                            self.collect_providers(fi, a.arg("providers"));
                        }
                        _ => {}
                    }
                }
            }
            // `providers: [...]` of `bootstrapApplication` / `ApplicationConfig`.
            for v in &f.values {
                if v.text.contains("provide:") {
                    self.collect_provider_text(fi, &v.text);
                }
            }
        }
        for (s, fi, name, line) in uses {
            for (t, c) in g.resolve_type(fi, &name) {
                g.add_edge(s, t, EdgeKind::Uses, 0.95 * c, line);
            }
        }
    }

    fn collect_providers(&mut self, fi: usize, arg: Option<&AnnotationArg>) {
        if let Some(a) = arg {
            self.collect_provider_text(fi, &a.value);
        }
    }

    /// `{provide: A, useClass: B}` / `useExisting: B` pairs of a provider list.
    fn collect_provider_text(&mut self, fi: usize, text: &str) {
        let mut rest = text;
        while let Some(i) = rest.find("provide:") {
            let after = &rest[i + "provide:".len()..];
            let end = after.find('}').unwrap_or(after.len());
            let obj = &after[..end];
            let token = obj.trim_start().split([',', ' ', '}']).next().unwrap_or("");
            for key in ["useClass:", "useExisting:"] {
                if let Some(j) = obj.find(key) {
                    let imp = obj[j + key.len()..].trim_start();
                    let imp: String = imp
                        .chars()
                        .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '$')
                        .collect();
                    if is_identifier(token) && !imp.is_empty() {
                        self.providers
                            .entry(token.to_string())
                            .or_default()
                            .push((fi, imp));
                    }
                }
            }
            rest = &after[end..];
        }
    }

    fn templates(&mut self, g: &mut GraphBuilder) {
        if self.selectors.is_empty() {
            return;
        }
        // HTML files: file → used components.
        let mut by_file: HashMap<usize, Vec<(SymIdx, f32, u32)>> = HashMap::new();
        for (fi, f) in self.index.files.iter().enumerate() {
            if f.lang != Lang::Html || g.symbol(g.file_symbol(fi)).has_role(Role::Test) {
                continue;
            }
            let from = g.file_symbol(fi);
            for e in &f.elements {
                let Some(comps) = self.selectors.get(&e.name) else {
                    continue;
                };
                let c = TEMPLATE / comps.len() as f32;
                for &t in comps {
                    g.add_edge(from, t, EdgeKind::Routes, c, e.line);
                    by_file.entry(fi).or_default().push((t, c, e.line));
                }
            }
        }
        for (owner, t) in std::mem::take(&mut self.templates) {
            match t {
                Template::File(fi, line) => {
                    for &(t, c, _) in by_file.get(&fi).into_iter().flatten() {
                        g.add_edge(owner, t, EdgeKind::Routes, c, line);
                    }
                }
                Template::Inline(text, line) => {
                    let body = unquote(&text).unwrap_or(&text);
                    for e in crate::index::html_elements(body) {
                        let Some(comps) = self.selectors.get(&e.name) else {
                            continue;
                        };
                        let c = TEMPLATE / comps.len() as f32;
                        for &t in comps {
                            g.add_edge(owner, t, EdgeKind::Routes, c, line + e.line - 1);
                        }
                    }
                }
            }
        }
    }

    // ---- routes

    fn routes(&mut self, g: &mut GraphBuilder) {
        let routed: Vec<usize> = self
            .files
            .iter()
            .copied()
            .filter(|&fi| !self.file(fi).routes.is_empty())
            .collect();
        if routed.is_empty() {
            return;
        }
        // Lazy loading: file → files whose routes it prefixes (with the route index).
        let mut lazy: Vec<(usize, u32, usize, SymIdx)> = Vec::new();
        for &fi in &routed {
            for (ri, r) in self.file(fi).routes.iter().enumerate() {
                let Some(load) = r.load.as_ref().filter(|l| l.children) else {
                    continue;
                };
                let Some(tf) = g.resolve_module(fi, &load.module) else {
                    continue;
                };
                let (target, targets) = self.lazy_targets(g, tf, load.export.as_deref());
                for t in targets {
                    lazy.push((fi, ri as u32, t, target));
                }
            }
        }
        // Prefixes of each routed file, to a fixpoint (lazy chains).
        let mut prefixes: HashMap<usize, Vec<String>> = HashMap::new();
        for _ in 0..6 {
            let mut next: HashMap<usize, Vec<String>> = HashMap::new();
            for &(fi, ri, tf, _) in &lazy {
                let base = prefixes
                    .get(&fi)
                    .cloned()
                    .unwrap_or_else(|| vec![String::new()]);
                for b in base {
                    let p = self.full_path(fi, ri, &b);
                    let v = next.entry(tf).or_default();
                    if !v.contains(&p) && v.len() < 8 {
                        v.push(p);
                    }
                }
            }
            if next == prefixes {
                break;
            }
            prefixes = next;
        }
        for &fi in &routed {
            let f = self.file(fi);
            let bases = prefixes
                .get(&fi)
                .cloned()
                .unwrap_or_else(|| vec![String::new()]);
            for (ri, r) in f.routes.iter().enumerate() {
                let owner = g.scope_symbol(fi, r.scope);
                let mut targets: Vec<(SymIdx, f32)> = Vec::new();
                if let Some(c) = &r.component {
                    targets.extend(
                        g.resolve_type(fi, c)
                            .into_iter()
                            .map(|(t, c)| (t, ROUTE * c)),
                    );
                }
                if let Some(load) = &r.load
                    && let Some(tf) = g.resolve_module(fi, &load.module)
                {
                    if load.children {
                        let (target, _) = self.lazy_targets(g, tf, load.export.as_deref());
                        g.add_edge(owner, target, EdgeKind::Routes, 0.9, r.line);
                    } else {
                        targets.extend(
                            self.exported_component(g, tf, load.export.as_deref())
                                .map(|t| (t, 0.9)),
                        );
                    }
                }
                for (t, c) in targets {
                    g.add_edge(owner, t, EdgeKind::Routes, c, r.line);
                    g.add_role(t, Role::EntryPoint);
                    add_tag(g, t, "entry", "route");
                    for b in &bases {
                        let p = self.full_path(fi, ri as u32, b);
                        add_tag(g, t, "angular.route", &format!("/{p}"));
                    }
                }
            }
        }
    }

    /// `path` of route `ri` of file `fi` joined with its parents' and `base`.
    fn full_path(&self, fi: usize, ri: u32, base: &str) -> String {
        let routes = &self.file(fi).routes;
        let mut parts = Vec::new();
        let mut r = Some(ri);
        while let Some(i) = r {
            parts.push(routes[i as usize].path.as_str());
            r = routes[i as usize].parent;
        }
        parts.push(base);
        parts.reverse();
        parts
            .iter()
            .flat_map(|p| p.split('/'))
            .filter(|p| !p.is_empty())
            .collect::<Vec<_>>()
            .join("/")
    }

    /// Target of `loadChildren` in file `tf`: the exported module (its routing
    /// modules' files) or routes constant; returns (symbol, files with routes).
    fn lazy_targets(
        &self,
        g: &GraphBuilder,
        tf: usize,
        export: Option<&str>,
    ) -> (SymIdx, Vec<usize>) {
        let f = self.file(tf);
        let mut files = vec![tf];
        let def = export.and_then(|e| {
            f.defs
                .iter()
                .position(|d| d.name == e && d.parent.is_none())
        });
        let Some(d) = def else {
            return (g.file_symbol(tf), files);
        };
        if let Some(m) = f.defs[d].annotation("NgModule")
            && let Some(imports) = m.arg("imports")
        {
            for n in identifiers(&imports.value) {
                for (t, _) in g.resolve_type(tf, n) {
                    if let Some(fi) = g.file_index(&g.symbol(t).file)
                        && !self.file(fi).routes.is_empty()
                        && !files.contains(&fi)
                    {
                        files.push(fi);
                    }
                }
            }
        }
        (g.def_symbol(tf, d), files)
    }

    /// `loadComponent` target: the named export, else the file's component.
    fn exported_component(
        &self,
        g: &GraphBuilder,
        tf: usize,
        export: Option<&str>,
    ) -> Option<SymIdx> {
        let f = self.file(tf);
        let d = match export {
            Some(e) => f
                .defs
                .iter()
                .position(|d| d.name == e && d.parent.is_none()),
            None => f
                .defs
                .iter()
                .position(|d| d.parent.is_none() && d.annotation("Component").is_some()),
        }?;
        Some(g.def_symbol(tf, d))
    }

    // ---- injection

    fn injection(&mut self, g: &mut GraphBuilder) {
        // (consumer, file, type or token name, line)
        let mut points: Vec<(SymIdx, usize, String, u32)> = Vec::new();
        for &fi in &self.files {
            let f = self.file(fi);
            for (d, def) in f.defs.iter().enumerate() {
                if def.kind != SymbolKind::Class || def.annotations.is_empty() {
                    continue;
                }
                let cls = g.def_symbol(fi, d);
                for (c, cdef) in members(f, d).filter(|(_, x)| x.kind == SymbolKind::Constructor) {
                    for b in f.bindings.iter().filter(|b| {
                        (b.scope == Some(c as u32) || (b.field && b.scope == Some(d as u32)))
                            && cdef.span.contains(b.line)
                    }) {
                        let token = b
                            .annotations
                            .iter()
                            .find(|a| a.name == "Inject")
                            .and_then(|a| a.value())
                            .map(|v| v.value.trim().to_string());
                        points.push((
                            cls,
                            fi,
                            token.unwrap_or_else(|| b.type_name.clone()),
                            b.line,
                        ));
                    }
                }
            }
            for r in &f.refs {
                if r.kind == RefKind::Call
                    && r.name == "inject"
                    && r.receiver.is_none()
                    && let Some(arg) = r.arg.as_deref().filter(|a| is_identifier(a))
                {
                    let owner = super::util::enclosing_type(f, r.scope)
                        .map(|d| d as u32)
                        .or(r.scope);
                    points.push((g.scope_symbol(fi, owner), fi, arg.to_string(), r.line));
                }
            }
        }
        for (from, fi, name, line) in points {
            for (t, c) in self.inject_targets(g, fi, &name) {
                g.add_edge(from, t, EdgeKind::Injects, c, line);
            }
        }
    }

    fn inject_targets(&self, g: &GraphBuilder, fi: usize, name: &str) -> Vec<(SymIdx, f32)> {
        // Provided implementation.
        if let Some(list) = self.providers.get(name) {
            let found: Vec<(SymIdx, f32)> = list
                .iter()
                .flat_map(|(pf, imp)| g.resolve_type(*pf, imp))
                .collect();
            if !found.is_empty() {
                let k = found.len() as f32;
                return found
                    .into_iter()
                    .map(|(t, c)| (t, INJECT * c / k))
                    .collect();
            }
        }
        let types = g.resolve_type(fi, name);
        if !types.is_empty() {
            return types.into_iter().map(|(t, c)| (t, INJECT * c)).collect();
        }
        // `InjectionToken` constant (same file or imported).
        self.constant(g, fi, name)
            .map(|t| (t, INJECT))
            .into_iter()
            .collect()
    }

    /// Top-level definition `name` of file `fi` or imported by it.
    fn constant(&self, g: &GraphBuilder, fi: usize, name: &str) -> Option<SymIdx> {
        let f = self.file(fi);
        if let Some(d) = f
            .defs
            .iter()
            .position(|d| d.name == name && d.parent.is_none())
        {
            return Some(g.def_symbol(fi, d));
        }
        let imp = f
            .imports
            .iter()
            .find(|i| i.names.iter().any(|n| n.local() == name))?;
        let n = imp.names.iter().find(|n| n.local() == name)?;
        let tf = g.resolve_module(fi, &imp.module)?;
        let d = self
            .file(tf)
            .defs
            .iter()
            .position(|d| d.name == n.name && d.parent.is_none())?;
        Some(g.def_symbol(tf, d))
    }

    // ---- environments

    fn environments(&mut self, g: &mut GraphBuilder) {
        let envs: Vec<usize> = self
            .files
            .iter()
            .copied()
            .filter(|&fi| {
                let p = &self.file(fi).path;
                p.rsplit('/')
                    .next()
                    .is_some_and(|n| n.starts_with("environment"))
            })
            .collect();
        for &e in &envs {
            let s = g.file_symbol(e);
            g.add_role(s, Role::Config);
            let name = self.file(e).path.rsplit('/').next().unwrap_or("");
            add_tag(g, s, "angular.environment", name.trim_end_matches(".ts"));
            for (d, def) in self.file(e).defs.iter().enumerate() {
                if def.parent.is_none() {
                    g.add_role(g.def_symbol(e, d), Role::Config);
                }
            }
        }
        if envs.is_empty() {
            return;
        }
        for &fi in &self.files {
            if envs.contains(&fi) {
                continue;
            }
            let f = self.file(fi);
            for imp in &f.imports {
                let Some(tf) = g.resolve_module(fi, &imp.module) else {
                    continue;
                };
                if !envs.contains(&tf) {
                    continue;
                }
                // The imported file and its variants (`environment.prod.ts`...).
                let dir = self.file(tf).path.rsplit_once('/').map_or("", |(d, _)| d);
                let variants: Vec<usize> = envs
                    .iter()
                    .copied()
                    .filter(|&v| self.file(v).path.rsplit_once('/').map_or("", |(d, _)| d) == dir)
                    .collect();
                for n in &imp.names {
                    let local = format!("{}.", n.local());
                    let mut consumers: Vec<(SymIdx, u32)> = f
                        .values
                        .iter()
                        .filter(|v| v.text.contains(&local))
                        .map(|v| (g.scope_symbol(fi, v.scope), v.line))
                        .collect();
                    consumers.extend(
                        f.refs
                            .iter()
                            .filter(|r| r.arg.as_deref().is_some_and(|a| a.contains(&local)))
                            .map(|r| (g.scope_symbol(fi, r.scope), r.line)),
                    );
                    if consumers.is_empty() {
                        consumers.push((g.file_symbol(fi), imp.line));
                    }
                    for &v in &variants {
                        let from = self
                            .file(v)
                            .defs
                            .iter()
                            .position(|d| d.name == n.name && d.parent.is_none())
                            .map_or_else(|| g.file_symbol(v), |d| g.def_symbol(v, d));
                        let conf = if v == tf { 0.9 } else { 0.6 };
                        let line = g.symbol(from).line;
                        for &(c, _) in &consumers {
                            g.add_edge(from, c, EdgeKind::Configures, conf, line);
                        }
                    }
                }
            }
        }
    }
}

/// Element selectors of a component selector: `app-x, [appY], app-z` →
/// `app-x`, `app-z`.
fn element_selectors(sel: &str) -> Vec<String> {
    split_top(sel, ',')
        .into_iter()
        .filter_map(|s| {
            let s = s.trim();
            let el: String = s
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
                .collect();
            (!el.is_empty() && el.starts_with(|c: char| c.is_ascii_alphabetic()))
                .then(|| el.to_ascii_lowercase())
        })
        .collect()
}
