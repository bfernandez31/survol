//! Helpers shared by the framework rules: walking annotated definitions,
//! typed names in scope, tags, and a small evaluator of string expressions
//! (URLs, paths) over the [`Value`]s of the index.

use std::collections::HashSet;

use crate::graph::{GraphBuilder, SymIdx};
use crate::index::{Annotation, Binding, Def, FileIndex, Lang, Value, unquote};

/// Appends `value` to tag `key` of `s` (`, `-separated, no duplicates).
pub fn add_tag(g: &mut GraphBuilder, s: SymIdx, key: &str, value: &str) {
    if value.is_empty() {
        return;
    }
    let tags = &mut g.symbol_mut(s).tags;
    match tags.get_mut(key) {
        Some(v) if v.split(", ").any(|x| x == value) => {}
        Some(v) => {
            v.push_str(", ");
            v.push_str(value);
        }
        None => {
            tags.insert(key.to_string(), value.to_string());
        }
    }
}

/// First annotation of `def` among `names`.
pub fn annotation<'a>(def: &'a Def, names: &[&str]) -> Option<&'a Annotation> {
    def.annotations
        .iter()
        .find(|a| names.contains(&a.name.as_str()))
}

pub fn has_annotation(annotations: &[Annotation], names: &[&str]) -> bool {
    annotations.iter().any(|a| names.contains(&a.name.as_str()))
}

/// Direct child definitions of `def` (members of a type).
pub fn members(f: &FileIndex, def: usize) -> impl Iterator<Item = (usize, &Def)> {
    f.defs
        .iter()
        .enumerate()
        .filter(move |(_, d)| d.parent == Some(def as u32))
}

/// Nearest type definition around `scope` (itself when it is a type).
pub fn enclosing_type(f: &FileIndex, scope: Option<u32>) -> Option<usize> {
    let mut s = scope;
    while let Some(d) = s {
        if f.defs[d as usize].kind.is_type() {
            return Some(d as usize);
        }
        s = f.defs[d as usize].parent;
    }
    None
}

/// Binding of `name` visible at `line` in `scope`: parameters and locals of
/// the enclosing callables, then fields of the enclosing types.
pub fn binding_at<'a>(
    f: &'a FileIndex,
    scope: Option<u32>,
    name: &str,
    line: u32,
    fields_only: bool,
) -> Option<&'a Binding> {
    let mut s = scope;
    while let Some(d) = s {
        let def = &f.defs[d as usize];
        let is_type = def.kind.is_type();
        if !fields_only || is_type {
            let found = f
                .bindings
                .iter()
                .rfind(|b| b.scope == Some(d) && b.name == name && (is_type || b.line <= line));
            if found.is_some() {
                return found;
            }
        }
        s = def.parent;
    }
    None
}

/// `receiver` of a call (`this.http`, `http`, `this?.http`) → the bound name
/// and whether it went through `this`.
pub fn receiver_name(receiver: &str) -> Option<(&str, bool)> {
    let (name, this) = match receiver
        .strip_prefix("this.")
        .or_else(|| receiver.strip_prefix("this?."))
    {
        Some(r) => (r, true),
        None => (receiver, false),
    };
    is_identifier(name).then_some((name, this))
}

pub fn is_identifier(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '$')
        && !s.starts_with(|c: char| c.is_ascii_digit())
}

/// Spring default bean name: `OwnerService` → `ownerService`.
pub fn decapitalize(name: &str) -> String {
    let mut c = name.chars();
    match c.next() {
        // `URLService` stays as is, like `Introspector.decapitalize`.
        Some(_) if name.chars().nth(1).is_some_and(char::is_uppercase) => name.to_string(),
        Some(f) => f.to_lowercase().chain(c).collect(),
        None => String::new(),
    }
}

/// Root of the build module holding `path`: the part before `/src/`, else
/// its directory. Used to prefer configuration of the same module.
pub fn module_root(path: &str) -> &str {
    if let Some(i) = path.find("/src/") {
        return &path[..i];
    }
    if path.starts_with("src/") {
        return "";
    }
    path.rsplit_once('/').map_or("", |(d, _)| d)
}

/// Splits `text` on `sep` outside quotes and brackets.
pub fn split_top(text: &str, sep: char) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    let mut start = 0;
    let mut prev = '\0';
    for (i, c) in text.char_indices() {
        match quote {
            Some(q) => {
                if c == q && prev != '\\' {
                    quote = None;
                }
            }
            None => match c {
                '"' | '\'' | '`' => quote = Some(c),
                '(' | '[' | '{' => depth += 1,
                ')' | ']' | '}' => depth -= 1,
                _ if c == sep && depth == 0 => {
                    out.push(&text[start..i]);
                    start = i + c.len_utf8();
                }
                _ => {}
            },
        }
        prev = c;
    }
    out.push(&text[start..]);
    out
}

/// Identifiers listed in an array argument: `[A, B, forwardRef(() => C)]`
/// → `A`, `B`, `C`; `RouterModule.forChild(x)` → `RouterModule`.
pub fn identifiers(text: &str) -> Vec<&str> {
    let t = text.trim();
    let t = t
        .strip_prefix('[')
        .and_then(|t| t.strip_suffix(']'))
        .or_else(|| t.strip_prefix('{').and_then(|t| t.strip_suffix('}')))
        .unwrap_or(t);
    split_top(t, ',')
        .into_iter()
        .filter_map(|item| {
            let item = item.trim();
            let item = item
                .strip_prefix("forwardRef(() =>")
                .map_or(item, |r| r.trim_end_matches(')').trim());
            let item = item.strip_prefix("...").unwrap_or(item);
            let head = item.split(['.', '(', ' ', '<']).next()?;
            (is_identifier(head) && head.starts_with(|c: char| c.is_ascii_uppercase()))
                .then_some(head)
        })
        .collect()
}

/// Type names in a class-literal list: `{A.class, B.class}`, `[A::class]`,
/// `A.class` → `A`, `B`.
pub fn class_literals(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    for sep in [".class", "::class"] {
        let mut rest = text;
        while let Some(i) = rest.find(sep) {
            let before = &rest[..i];
            let start = before
                .rfind(|c: char| !(c.is_alphanumeric() || c == '_' || c == '.'))
                .map_or(0, |p| p + 1);
            let name = before[start..].rsplit('.').next().unwrap_or("");
            if !name.is_empty() {
                out.push(name);
            }
            rest = &rest[i + sep.len()..];
        }
    }
    out
}

/// Placeholders of a Spring property expression: `${a.b:def}` → `a.b`.
pub fn placeholders(text: &str) -> Vec<String> {
    let text = text.replace("\\$", "$");
    let mut out = Vec::new();
    let mut rest = text.as_str();
    while let Some(i) = rest.find("${") {
        let body = &rest[i + 2..];
        let end = body.find(['}', ':']).unwrap_or(body.len());
        let key = body[..end].trim();
        if !key.is_empty() && !key.contains("${") {
            out.push(key.to_string());
        }
        rest = &body[end..];
    }
    out
}

/// Relaxed-binding form of a configuration key: lower case, no `-`/`_`,
/// no list indices (`my-app.Items[0].url` → `myapp.items.url`).
pub fn relaxed_key(key: &str) -> String {
    let mut out = String::with_capacity(key.len());
    let mut in_index = false;
    for c in key.chars() {
        match c {
            '[' => in_index = true,
            ']' => in_index = false,
            _ if in_index => {}
            '-' | '_' => {}
            c => out.extend(c.to_lowercase()),
        }
    }
    out
}

/// Part of a string built from an expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Part {
    Lit(String),
    /// Unknown at analysis time (a parameter, a call...).
    Wild,
}

/// Evaluates a string expression as written at `line` in `scope` of head
/// file `fi`: literals, concatenations, template literals and Kotlin string
/// templates, names resolved through [`Value`]s (locals, fields, constants,
/// imported constants, `Type.CONST`, `object.KEY` of an object literal such as
/// `environment`). Whatever cannot be resolved becomes [`Part::Wild`].
pub fn eval(g: &GraphBuilder, fi: usize, scope: Option<u32>, line: u32, expr: &str) -> Vec<Part> {
    let mut out = Vec::new();
    Eval {
        g,
        index: g.index(),
        seen: HashSet::new(),
    }
    .expr(fi, scope, line, expr, 0, &mut out);
    merge(out)
}

/// Adjacent literals joined, adjacent wildcards collapsed.
fn merge(parts: Vec<Part>) -> Vec<Part> {
    let mut out: Vec<Part> = Vec::new();
    for p in parts {
        match (out.last_mut(), p) {
            (Some(Part::Lit(a)), Part::Lit(b)) => a.push_str(&b),
            (Some(Part::Wild), Part::Wild) => {}
            (_, p) => out.push(p),
        }
    }
    out
}

struct Eval<'g, 'a> {
    g: &'g GraphBuilder<'a>,
    index: &'a crate::index::Index,
    /// (file, value line, name) being evaluated: breaks cycles.
    seen: HashSet<(usize, u32, String)>,
}

const MAX_DEPTH: usize = 6;

impl<'a> Eval<'_, 'a> {
    fn expr(
        &mut self,
        fi: usize,
        scope: Option<u32>,
        line: u32,
        expr: &str,
        depth: usize,
        out: &mut Vec<Part>,
    ) {
        if depth > MAX_DEPTH {
            out.push(Part::Wild);
            return;
        }
        for term in split_top(expr, '+') {
            let term = term.trim();
            let t = strip_parens(term);
            if t.is_empty() {
                continue;
            }
            if t.len() != term.len() {
                // `(a + b)`: a nested concatenation.
                self.expr(fi, scope, line, t, depth + 1, out);
            } else {
                self.term(fi, scope, line, t, depth, out);
            }
        }
    }

    fn term(
        &mut self,
        fi: usize,
        scope: Option<u32>,
        line: u32,
        t: &str,
        depth: usize,
        out: &mut Vec<Part>,
    ) {
        let lang = self.index.files[fi].lang;
        if let Some(body) = t.strip_prefix('`').and_then(|b| b.strip_suffix('`')) {
            self.template(fi, scope, line, body, "${", depth, out);
            return;
        }
        if let Some(body) = t
            .strip_prefix("\"\"\"")
            .and_then(|b| b.strip_suffix("\"\"\""))
        {
            out.push(Part::Lit(body.to_string()));
            return;
        }
        if let Some(body) = unquote(t) {
            if lang == Lang::Kotlin && t.starts_with('"') && body.contains('$') {
                self.template(fi, scope, line, body, "$", depth, out);
            } else {
                out.push(Part::Lit(unescape(body)));
            }
            return;
        }
        // `a ?: b`, `a || b`, `a ?? b`: the first alternative.
        for alt in ["?:", "||", "??"] {
            if let Some((a, _)) = t.split_once(alt) {
                self.term(fi, scope, line, a.trim(), depth + 1, out);
                return;
            }
        }
        let name = t.strip_prefix("this.").unwrap_or(t);
        let path: Vec<&str> = name
            .split(['.'])
            .map(|s| s.trim_end_matches(['!', '?']))
            .collect();
        if !path.iter().all(|p| is_identifier(p)) {
            out.push(Part::Wild);
            return;
        }
        match self.lookup(fi, scope, line, &path, t.starts_with("this."), depth) {
            Some(parts) => out.extend(parts),
            None => out.push(Part::Wild),
        }
    }

    /// Template literal / Kotlin string template body.
    #[allow(clippy::too_many_arguments)]
    fn template(
        &mut self,
        fi: usize,
        scope: Option<u32>,
        line: u32,
        body: &str,
        open: &str,
        depth: usize,
        out: &mut Vec<Part>,
    ) {
        let body = body.replace("\\$", "\u{1}");
        let mut rest = body.as_str();
        while let Some(i) = rest.find('$') {
            out.push(Part::Lit(rest[..i].replace('\u{1}', "$")));
            let after = &rest[i + 1..];
            if let Some(inner) = after.strip_prefix('{') {
                let end = inner.find('}').unwrap_or(inner.len());
                self.expr(fi, scope, line, &inner[..end], depth + 1, out);
                rest = inner.get(end + 1..).unwrap_or("");
            } else if open == "$" {
                let end = after
                    .find(|c: char| !(c.is_alphanumeric() || c == '_'))
                    .unwrap_or(after.len());
                if end == 0 {
                    out.push(Part::Lit("$".into()));
                } else {
                    self.expr(fi, scope, line, &after[..end], depth + 1, out);
                }
                rest = &after[end..];
            } else {
                out.push(Part::Lit("$".into()));
                rest = after;
            }
        }
        out.push(Part::Lit(rest.replace('\u{1}', "$")));
    }

    /// Value of a dotted name: `x`, `this.x`, `environment.API`, `Paths.BASE`.
    fn lookup(
        &mut self,
        fi: usize,
        scope: Option<u32>,
        line: u32,
        path: &[&str],
        via_this: bool,
        depth: usize,
    ) -> Option<Vec<Part>> {
        let f = &self.index.files[fi];
        let (head, rest) = path.split_first()?;
        // A value in scope (locals, fields, top-level constants of the file).
        if let Some((vf, v)) = self.value_in_scope(fi, scope, line, head, via_this) {
            return self.value_parts(vf, v, rest, depth);
        }
        // An imported constant (TS/JS) or a static constant (`Type.CONST`).
        if !f.lang.is_jvm() {
            for imp in &f.imports {
                if let Some(n) = imp.names.iter().find(|n| n.local() == *head) {
                    let target = self.g.resolve_module(fi, &imp.module)?;
                    let v = top_value(&self.index.files[target], &n.name)?;
                    return self.value_parts(target, v, rest, depth);
                }
            }
            return None;
        }
        let (member, _) = rest.split_first()?;
        for (t, _) in self.g.resolve_type(fi, head) {
            let sym = self.g.symbol(t);
            let tf = self.g.file_index(&sym.file)?;
            let tfile = &self.index.files[tf];
            let def = (0..tfile.defs.len()).find(|&d| self.g.def_symbol(tf, d) == t)? as u32;
            // Constants of the type or of its companion object.
            if let Some(v) = tfile.values.iter().find(|v| {
                v.name == *member
                    && v.scope
                        .is_some_and(|s| s == def || tfile.defs[s as usize].parent == Some(def))
            }) {
                return self.value_parts(tf, v, &rest[1..], depth);
            }
        }
        None
    }

    /// Value `name` visible at `line` in `scope`: locals of the enclosing
    /// callables (declared before), fields of the enclosing types, then the
    /// file's top-level constants.
    fn value_in_scope(
        &self,
        fi: usize,
        scope: Option<u32>,
        line: u32,
        name: &str,
        fields_only: bool,
    ) -> Option<(usize, &'a Value)> {
        let f = &self.index.files[fi];
        let mut s = scope;
        while let Some(d) = s {
            let def = &f.defs[d as usize];
            let is_type = def.kind.is_type();
            if (!fields_only || is_type)
                && let Some(v) = f
                    .values
                    .iter()
                    .rfind(|v| v.scope == Some(d) && v.name == name && (is_type || v.line <= line))
            {
                return Some((fi, v));
            }
            s = def.parent;
        }
        if fields_only {
            return None;
        }
        top_value(f, name).map(|v| (fi, v))
    }

    /// Parts of value `v` (of file `fi`), or of its property path `rest`
    /// when it is an object literal.
    fn value_parts(
        &mut self,
        fi: usize,
        v: &Value,
        rest: &[&str],
        depth: usize,
    ) -> Option<Vec<Part>> {
        let key = (fi, v.line, v.name.clone());
        if !self.seen.insert(key.clone()) {
            return None;
        }
        let mut text = v.text.as_str();
        for p in rest {
            text = object_property(text, p)?;
        }
        let mut out = Vec::new();
        self.expr(fi, v.scope, v.line, text, depth + 1, &mut out);
        self.seen.remove(&key);
        // `x = ''`: a default, set later.
        if out
            .iter()
            .all(|p| matches!(p, Part::Lit(l) if l.is_empty()))
        {
            return None;
        }
        Some(out)
    }
}

/// Top-level value `name` of a file.
pub fn top_value<'a>(f: &'a FileIndex, name: &str) -> Option<&'a Value> {
    f.values
        .iter()
        .find(|v| v.scope.is_none() && v.name == name)
}

/// Value of `key` in an object literal: `{a: 'x', b: 1}` + `a` → `'x'`.
pub fn object_property<'a>(object: &'a str, key: &str) -> Option<&'a str> {
    let body = object.trim().strip_prefix('{')?.strip_suffix('}')?;
    split_top(body, ',').into_iter().find_map(|pair| {
        let (k, v) = pair.split_once(':')?;
        let k = k.trim();
        (unquote(k).unwrap_or(k) == key).then(|| v.trim())
    })
}

/// `\/`, `\'`, `\"` → `/`, `'`, `"`.
fn unescape(s: &str) -> String {
    s.replace("\\/", "/")
        .replace("\\'", "'")
        .replace("\\\"", "\"")
}

fn strip_parens(t: &str) -> &str {
    let mut t = t;
    while let Some(inner) = t.strip_prefix('(').and_then(|x| x.strip_suffix(')')) {
        t = inner.trim();
    }
    t
}
