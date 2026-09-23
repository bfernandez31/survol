//! One file → [`FileIndex`]: runs the language query, then reads what the
//! query cannot express (annotations, containers, arity, imports) from the tree.

use std::collections::HashMap;

use streaming_iterator::StreamingIterator;
use tree_sitter::{Node, Parser, QueryCursor};

use super::{
    Annotation, AnnotationArg, Binding, Def, FileIndex, Import, ImportedName, Lang, Ref, RefKind,
    Span, SymbolKind,
};

/// Receivers longer than this are cut (chained calls can be huge).
const MAX_RECEIVER: usize = 80;

/// Kinds of the nodes holding annotations or decorators.
const ANNOTATION_KINDS: &[&str] = &["annotation", "marker_annotation", "decorator"];

/// Parameter nodes, in all grammars (JS patterns are bare in `formal_parameters`).
const PARAM_KINDS: &[&str] = &[
    "formal_parameter",
    "spread_parameter",
    "parameter",
    "class_parameter",
    "required_parameter",
    "optional_parameter",
    "identifier",
    "assignment_pattern",
    "rest_pattern",
    "object_pattern",
    "array_pattern",
];

/// Parses `src` and extracts its definitions, references, imports and bindings.
/// Never fails: syntax errors give a partial result with `has_errors` set.
pub fn parse_file(path: &str, lang: Lang, src: &str) -> FileIndex {
    let mut parser = Parser::new();
    parser
        .set_language(&lang.grammar())
        .expect("grammar compatible with the tree-sitter version");
    let mut out = FileIndex {
        path: path.to_string(),
        lang,
        package: None,
        defs: Vec::new(),
        refs: Vec::new(),
        imports: Vec::new(),
        bindings: Vec::new(),
        lines: src.lines().count() as u32,
        has_errors: false,
    };
    let Some(tree) = parser.parse(src, None) else {
        out.has_errors = true;
        return out;
    };
    let root = tree.root_node();
    out.has_errors = root.has_error();
    Extractor {
        lang,
        src,
        out: &mut out,
    }
    .run(root);
    out
}

struct Extractor<'a> {
    lang: Lang,
    src: &'a str,
    out: &'a mut FileIndex,
}

/// A primary capture: the node a pattern is about, with its helpers.
struct Hit<'t> {
    pattern: usize,
    what: String,
    node: Node<'t>,
    name: Option<Node<'t>>,
    receiver: Option<Node<'t>>,
    binding_type: Option<Node<'t>>,
}

impl<'a> Extractor<'a> {
    fn text(&self, n: Node) -> &'a str {
        n.utf8_text(self.src.as_bytes()).unwrap_or("")
    }

    fn run(&mut self, root: Node) {
        let query = self.lang.query();
        let names = query.capture_names();
        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(query, root, self.src.as_bytes());

        // When several patterns capture the same node, the first pattern wins.
        let mut defs: HashMap<usize, Hit> = HashMap::new();
        let mut refs: HashMap<usize, Hit> = HashMap::new();
        let mut bindings: HashMap<usize, Hit> = HashMap::new();
        let mut imports = Vec::new();
        while let Some(m) = matches.next() {
            let mut hit: Option<Hit> = None;
            let (mut name, mut receiver, mut btype, mut bname) = (None, None, None, None);
            for c in m.captures() {
                match names[c.index as usize] {
                    "name" => name = Some(c.node),
                    "receiver" => receiver = Some(c.node),
                    "binding.type" => btype = Some(c.node),
                    "binding.name" => bname = Some(c.node),
                    "package" => self.out.package = Some(self.text(c.node).to_string()),
                    "import" => imports.push(c.node),
                    what if what.starts_with("definition.")
                        || what.starts_with("reference.")
                        || what.starts_with("binding.") =>
                    {
                        hit = Some(Hit {
                            pattern: m.pattern_index,
                            what: what.to_string(),
                            node: c.node,
                            name: None,
                            receiver: None,
                            binding_type: None,
                        });
                    }
                    _ => {}
                }
            }
            let Some(mut hit) = hit else { continue };
            hit.receiver = receiver;
            let (map, key) = if hit.what.starts_with("definition.") {
                hit.name = name;
                (&mut defs, hit.node.id())
            } else if hit.what.starts_with("reference.") {
                let Some(n) = name else { continue };
                hit.name = Some(n);
                (&mut refs, n.id())
            } else {
                let (Some(n), Some(t)) = (bname, btype) else {
                    continue;
                };
                hit.name = Some(n);
                hit.binding_type = Some(t);
                (&mut bindings, n.id())
            };
            match map.get(&key) {
                Some(prev) if prev.pattern <= hit.pattern => {}
                _ => {
                    map.insert(key, hit);
                }
            }
        }

        let mut def_hits: Vec<Hit> = defs.into_values().collect();
        // Outer definitions first, so that parents come before children.
        def_hits.sort_by_key(|h| (h.node.start_byte(), std::cmp::Reverse(h.node.end_byte())));
        let ranges = self.defs(&def_hits);
        let mut refs: Vec<Hit> = refs.into_values().collect();
        refs.sort_by_key(|h| h.name.map_or(0, |n| n.start_byte()));
        for h in refs {
            self.reference(h, &ranges);
        }
        let mut bindings: Vec<Hit> = bindings.into_values().collect();
        bindings.sort_by_key(|h| h.node.start_byte());
        for h in bindings {
            self.binding(h, &ranges);
        }
        imports.sort_by_key(|n| n.start_byte());
        imports.dedup_by_key(|n| n.id());
        for n in imports {
            if let Some(i) = self.import(n) {
                self.out.imports.push(i);
            }
        }
    }

    /// Fills `out.defs` from the sorted definition hits; returns their byte ranges.
    fn defs(&mut self, hits: &[Hit]) -> Vec<(usize, usize)> {
        let mut ranges: Vec<(usize, usize)> = Vec::new();
        let mut stack: Vec<usize> = Vec::new();
        for h in hits {
            let (start, end) = (h.node.start_byte(), h.node.end_byte());
            while let Some(&top) = stack.last() {
                if ranges[top].1 >= end && ranges[top].0 <= start {
                    break;
                }
                stack.pop();
            }
            let parent = stack.last().copied();
            let parent_kind = parent.map(|p| self.out.defs[p].kind);
            let mut kind = def_kind(&h.what);
            if kind == SymbolKind::Function && parent_kind.is_some_and(SymbolKind::is_type) {
                kind = SymbolKind::Method;
            }
            let mut name = h.name.map(|n| self.text(n).to_string()).unwrap_or_default();
            if kind == SymbolKind::Method && name == "constructor" {
                kind = SymbolKind::Constructor;
            }
            if name.is_empty() {
                name = match kind {
                    SymbolKind::Constructor => parent
                        .map(|p| self.out.defs[p].name.clone())
                        .unwrap_or_default(),
                    SymbolKind::Object => "Companion".into(),
                    _ => continue,
                };
            }
            let annotations = self.annotations(h.node);
            let mut span = span(h.node);
            if let Some(first) = annotations.iter().map(|a| a.line).min() {
                span.start = span.start.min(first);
            }
            let (params, variadic) =
                if kind.is_callable() || matches!(kind, SymbolKind::Class | SymbolKind::Record) {
                    self.params(h.node)
                } else {
                    (None, false)
                };
            self.out.defs.push(Def {
                name,
                kind,
                span,
                line: h.name.map_or(span.start, |n| line(n)),
                parent: parent.map(|p| p as u32),
                annotations,
                params,
                variadic,
            });
            ranges.push((start, end));
            stack.push(ranges.len() - 1);
        }
        ranges
    }

    /// Innermost definition containing `node`, skipping fields when `skip_fields`.
    fn scope(&self, node: Node, ranges: &[(usize, usize)], skip_fields: bool) -> Option<u32> {
        let (s, e) = (node.start_byte(), node.end_byte());
        ranges
            .iter()
            .enumerate()
            .filter(|(i, (rs, re))| {
                *rs <= s
                    && e <= *re
                    && !(skip_fields && self.out.defs[*i].kind == SymbolKind::Field)
            })
            .min_by_key(|(_, (rs, re))| re - rs)
            .map(|(i, _)| i as u32)
    }

    fn reference(&mut self, h: Hit, ranges: &[(usize, usize)]) {
        let name_node = h.name.expect("references have a name");
        // `@Component({...})`: the decorator is an annotation, not a call.
        if h.node.parent().is_some_and(|p| p.kind() == "decorator") {
            return;
        }
        let kind = match h.what.as_str() {
            "reference.call" => RefKind::Call,
            "reference.new" => RefKind::New,
            "reference.extends" => RefKind::Extends,
            "reference.implements" => RefKind::Implements,
            _ => RefKind::Type,
        };
        let name = type_name(self.text(name_node));
        if name.is_empty() {
            return;
        }
        let receiver = h.receiver.map(|r| {
            let t: String = self
                .text(r)
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            match t.char_indices().nth(MAX_RECEIVER) {
                Some((i, _)) => t[..i].to_string(),
                None => t,
            }
        });
        let arity = match kind {
            RefKind::Call | RefKind::New => arity(h.node),
            _ => None,
        };
        self.out.refs.push(Ref {
            kind,
            name,
            receiver,
            arity,
            line: line(name_node),
            scope: self.scope(h.node, ranges, false),
        });
    }

    fn binding(&mut self, h: Hit, ranges: &[(usize, usize)]) {
        let name = self.text(h.name.expect("bindings have a name")).to_string();
        let type_name = type_name(self.text(h.binding_type.expect("bindings have a type")));
        // Inferred from a call: only constructor calls (capitalised) count.
        if type_name.is_empty() || !type_name.starts_with(|c: char| c.is_ascii_uppercase()) {
            return;
        }
        let field = h.what == "binding.field";
        let mut scope = self.scope(h.node, ranges, true);
        if field {
            while let Some(s) = scope {
                let d = &self.out.defs[s as usize];
                if d.kind.is_type() {
                    break;
                }
                scope = d.parent;
            }
        }
        self.out.bindings.push(Binding {
            name,
            type_name,
            scope,
            field,
            line: line(h.node),
        });
    }

    /// Annotations and decorators of a definition node: in the node, in its
    /// `modifiers`, before it (TypeScript members) and on a wrapping `export`.
    fn annotations(&self, node: Node) -> Vec<Annotation> {
        let mut nodes = Vec::new();
        let mut c = node.walk();
        for ch in node.children(&mut c) {
            if ANNOTATION_KINDS.contains(&ch.kind()) {
                nodes.push(ch);
            } else if ch.kind() == "modifiers" {
                let mut c2 = ch.walk();
                nodes.extend(
                    ch.children(&mut c2)
                        .filter(|m| ANNOTATION_KINDS.contains(&m.kind())),
                );
            }
        }
        let mut prev = node.prev_named_sibling();
        while let Some(p) = prev {
            match p.kind() {
                "decorator" => nodes.push(p),
                "comment" => {}
                _ => break,
            }
            prev = p.prev_named_sibling();
        }
        if let Some(parent) = node.parent().filter(|p| p.kind() == "export_statement") {
            let mut c = parent.walk();
            nodes.extend(parent.children(&mut c).filter(|n| n.kind() == "decorator"));
        }
        nodes.sort_by_key(|n| n.start_byte());
        nodes.dedup_by_key(|n| n.id());
        nodes
            .into_iter()
            .filter_map(|n| self.annotation(n))
            .collect()
    }

    fn annotation(&self, node: Node) -> Option<Annotation> {
        let mut name = String::new();
        let mut args = Vec::new();
        match node.kind() {
            // Java
            "marker_annotation" | "annotation" if self.lang == Lang::Java => {
                name = self.text(node.child_by_field_name("name")?).to_string();
                if let Some(list) = node.child_by_field_name("arguments") {
                    for a in named_children(list) {
                        args.push(if a.kind() == "element_value_pair" {
                            AnnotationArg {
                                key: a.child_by_field_name("key").map(|k| self.text(k).into()),
                                value: a
                                    .child_by_field_name("value")
                                    .map(|v| self.text(v).into())
                                    .unwrap_or_default(),
                            }
                        } else {
                            AnnotationArg {
                                key: None,
                                value: self.text(a).into(),
                            }
                        });
                    }
                }
            }
            // Kotlin: `@Foo` (user_type) or `@Foo(args)` (constructor_invocation).
            "annotation" => {
                for ch in named_children(node) {
                    match ch.kind() {
                        "user_type" => name = self.text(ch).into(),
                        "constructor_invocation" => {
                            for part in named_children(ch) {
                                match part.kind() {
                                    "user_type" => name = self.text(part).into(),
                                    "value_arguments" => {
                                        for a in named_children(part) {
                                            args.push(self.kotlin_arg(a));
                                        }
                                    }
                                    _ => {}
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            // TypeScript / JavaScript
            _ => {
                let expr = named_children(node).next()?;
                let callee = if expr.kind() == "call_expression" {
                    if let Some(list) = expr.child_by_field_name("arguments") {
                        let list: Vec<Node> = named_children(list)
                            .filter(|n| n.kind() != "comment")
                            .collect();
                        if let [obj] = list.as_slice()
                            && obj.kind() == "object"
                        {
                            args.extend(self.object_pairs(*obj));
                        } else {
                            args.extend(list.iter().map(|a| AnnotationArg {
                                key: None,
                                value: self.text(*a).into(),
                            }));
                        }
                    }
                    expr.child_by_field_name("function")?
                } else {
                    expr
                };
                name = self.text(callee).into();
            }
        }
        let name = type_name(&name);
        (!name.is_empty()).then(|| Annotation {
            name,
            args,
            line: line(node),
        })
    }

    fn kotlin_arg(&self, arg: Node) -> AnnotationArg {
        let parts: Vec<Node> = named_children(arg).collect();
        match parts.as_slice() {
            [k, v] if k.kind() == "identifier" => AnnotationArg {
                key: Some(self.text(*k).into()),
                value: self.text(*v).into(),
            },
            _ => AnnotationArg {
                key: None,
                value: self.text(arg).into(),
            },
        }
    }

    fn object_pairs(&self, obj: Node) -> Vec<AnnotationArg> {
        named_children(obj)
            .filter_map(|p| match p.kind() {
                "pair" => {
                    let k = self.text(p.child_by_field_name("key")?);
                    Some(AnnotationArg {
                        key: Some(super::unquote(k).unwrap_or(k).to_string()),
                        value: self.text(p.child_by_field_name("value")?).into(),
                    })
                }
                "shorthand_property_identifier" => Some(AnnotationArg {
                    key: Some(self.text(p).into()),
                    value: self.text(p).into(),
                }),
                _ => None,
            })
            .collect()
    }

    /// Declared parameter count, and whether the last one is variadic.
    fn params(&self, node: Node) -> (Option<u16>, bool) {
        let list = params_node(node);
        let Some(list) = list else {
            return (None, false);
        };
        if list.kind() == "identifier" {
            return (Some(1), false); // `x => ...`
        }
        let mut n = 0;
        let mut variadic = false;
        for p in named_children(list) {
            match p.kind() {
                "parameter_modifiers" => variadic |= self.text(p).contains("vararg"),
                k if PARAM_KINDS.contains(&k) => {
                    n += 1;
                    variadic |= k == "spread_parameter"
                        || k == "rest_pattern"
                        || p.child_by_field_name("pattern")
                            .is_some_and(|pat| pat.kind() == "rest_pattern");
                }
                _ => {}
            }
        }
        (Some(n), variadic)
    }

    fn import(&self, node: Node) -> Option<Import> {
        let mut imp = Import {
            module: String::new(),
            names: Vec::new(),
            wildcard: false,
            alias: None,
            is_static: false,
            line: line(node),
        };
        match self.lang {
            Lang::Java => {
                let mut c = node.walk();
                let mut path = "";
                for ch in node.children(&mut c) {
                    match ch.kind() {
                        "static" => imp.is_static = true,
                        "asterisk" => imp.wildcard = true,
                        "scoped_identifier" | "identifier" => path = self.text(ch),
                        _ => {}
                    }
                }
                split_import(&mut imp, path);
            }
            Lang::Kotlin => {
                let mut path = "";
                let mut alias = None;
                for ch in named_children(node) {
                    match ch.kind() {
                        "qualified_identifier" | "identifier" if path.is_empty() => {
                            path = self.text(ch)
                        }
                        "identifier" => alias = Some(self.text(ch).to_string()),
                        _ => {}
                    }
                }
                imp.wildcard = self.text(node).trim_end().ends_with('*');
                split_import(&mut imp, path);
                if let Some(n) = imp.names.first_mut() {
                    n.alias = alias;
                }
            }
            _ => {
                if node.kind() == "call_expression" {
                    // require('x')
                    let arg = named_children(node.child_by_field_name("arguments")?).next()?;
                    imp.module = self.string(arg)?;
                    imp.wildcard = true;
                    return Some(imp);
                }
                imp.module = self.string(node.child_by_field_name("source")?)?;
                for ch in named_children(node) {
                    match ch.kind() {
                        "import_clause" => {
                            for part in named_children(ch) {
                                match part.kind() {
                                    "identifier" => imp.names.push(ImportedName {
                                        name: "default".into(),
                                        alias: Some(self.text(part).into()),
                                    }),
                                    "named_imports" => imp.names.extend(self.specifiers(part)),
                                    "namespace_import" => {
                                        imp.wildcard = true;
                                        imp.alias = named_children(part)
                                            .next()
                                            .map(|n| self.text(n).into());
                                    }
                                    _ => {}
                                }
                            }
                        }
                        "export_clause" => imp.names.extend(self.specifiers(ch)),
                        "namespace_export" => imp.wildcard = true,
                        _ => {}
                    }
                }
                if node.kind() == "export_statement"
                    && imp.names.is_empty()
                    && self.text(node).contains('*')
                {
                    imp.wildcard = true;
                }
            }
        }
        (!imp.module.is_empty() || !imp.names.is_empty()).then_some(imp)
    }

    fn specifiers(&self, list: Node) -> Vec<ImportedName> {
        named_children(list)
            .filter_map(|s| {
                let name = s.child_by_field_name("name")?;
                Some(ImportedName {
                    name: self.string(name).unwrap_or_else(|| self.text(name).into()),
                    alias: s.child_by_field_name("alias").map(|a| self.text(a).into()),
                })
            })
            .collect()
    }

    /// Content of a string literal node.
    fn string(&self, n: Node) -> Option<String> {
        super::unquote(self.text(n)).map(str::to_string)
    }
}

/// `a.b.C` → module `a.b`, name `C` (or the whole path for a wildcard).
fn split_import(imp: &mut Import, path: &str) {
    if imp.wildcard {
        imp.module = path.to_string();
        return;
    }
    match path.rsplit_once('.') {
        Some((module, name)) => {
            imp.module = module.to_string();
            imp.names.push(ImportedName {
                name: name.to_string(),
                alias: None,
            });
        }
        None => imp.module = path.to_string(),
    }
}

fn def_kind(what: &str) -> SymbolKind {
    match what.trim_start_matches("definition.") {
        "interface" => SymbolKind::Interface,
        "enum" => SymbolKind::Enum,
        "record" => SymbolKind::Record,
        "object" => SymbolKind::Object,
        "annotation" => SymbolKind::Annotation,
        "method" => SymbolKind::Method,
        "function" => SymbolKind::Function,
        "constructor" => SymbolKind::Constructor,
        "field" => SymbolKind::Field,
        _ => SymbolKind::Class,
    }
}

/// The node holding the parameters of a definition.
fn params_node(node: Node) -> Option<Node> {
    if let Some(p) = node
        .child_by_field_name("parameters")
        .or_else(|| node.child_by_field_name("parameter"))
    {
        return Some(p);
    }
    // `const f = (a) => ...`, `f = function (a) {}` (TS field): on the value.
    if let Some(v) = node.child_by_field_name("value")
        && matches!(v.kind(), "arrow_function" | "function_expression")
    {
        return params_node(v);
    }
    named_children(node).find_map(|ch| match ch.kind() {
        "formal_parameters" | "function_value_parameters" | "class_parameters" => Some(ch),
        "primary_constructor" => named_children(ch).find(|p| p.kind() == "class_parameters"),
        _ => None,
    })
}

/// Arguments of a call node; Kotlin trailing lambdas count as one.
fn arity(call: Node) -> Option<u16> {
    let mut n = 0;
    let mut found = false;
    for ch in named_children(call) {
        match ch.kind() {
            "argument_list" | "arguments" | "value_arguments" => {
                found = true;
                n += named_children(ch).filter(|a| a.kind() != "comment").count() as u16;
            }
            "annotated_lambda" => {
                found = true;
                n += 1;
            }
            _ => {}
        }
    }
    found.then_some(n)
}

/// `a.b.C<T>?`, `C[]`, `Foo(1)` → `C`, `Foo`.
fn type_name(text: &str) -> String {
    let t = text
        .split(['<', '(', '[', '?', '{'])
        .next()
        .unwrap_or("")
        .trim();
    t.rsplit(['.', ':']).next().unwrap_or("").trim().to_string()
}

fn named_children(node: Node) -> impl Iterator<Item = Node> {
    (0..node.named_child_count()).filter_map(move |i| node.named_child(i as u32))
}

fn line(n: Node) -> u32 {
    n.start_position().row as u32 + 1
}

fn span(n: Node) -> Span {
    Span {
        start: line(n),
        end: n.end_position().row as u32 + 1,
    }
}
