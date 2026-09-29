//! The file list of the Diff view, in three modes (`m`):
//!
//! - **tree**: a compact, foldable tree. Modules, then their source sets
//!   (`main`, `test`, `openapi`...) as separate branches, the package root
//!   they share shown once; a chain of directories with a single child is
//!   one node, a directory with a single file is merged into the file's line;
//! - **pairs**: each changed class with the tests changed with it beneath
//!   (matched by name, then by the test edges of the code graph), classes
//!   without a changed test flagged;
//! - **flat**: one line per file, the name first, where it lives after it.

use std::collections::{HashMap, HashSet};

/// How the files are listed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Mode {
    #[default]
    Tree,
    Pairs,
    Flat,
}

impl Mode {
    pub fn name(self) -> &'static str {
        match self {
            Mode::Tree => "tree",
            Mode::Pairs => "pairs",
            Mode::Flat => "flat",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        [Mode::Tree, Mode::Pairs, Mode::Flat]
            .into_iter()
            .find(|m| m.name() == s)
    }

    pub fn next(self) -> Self {
        match self {
            Mode::Tree => Mode::Pairs,
            Mode::Pairs => Mode::Flat,
            Mode::Flat => Mode::Tree,
        }
    }
}

/// What a file is, in the pairs mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Role {
    Plain,
    /// Source code; `tested`: a changed test goes with it.
    Class {
        tested: bool,
    },
    /// A test under its class; `by_graph`: matched by the code graph.
    Test {
        by_graph: bool,
    },
}

/// A line of the file list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SideItem {
    Dir {
        /// Identifies the directory across rebuilds (for folds).
        key: String,
        depth: u16,
        label: String,
        /// Shown dimmed after the label (the package root).
        note: String,
        /// Every file under it.
        files: Vec<usize>,
    },
    File {
        file: usize,
        depth: u16,
        /// Directories merged into the line, shown dimmed before the name.
        prefix: String,
        role: Role,
        /// Where the file lives (flat mode).
        place: String,
    },
}

impl SideItem {
    pub fn depth(&self) -> u16 {
        match self {
            SideItem::Dir { depth, .. } | SideItem::File { depth, .. } => *depth,
        }
    }
}

// ----- paths ----------------------------------------------------------------

/// Directories holding code in a source set: their files have packages.
const CODE_DIRS: &[&str] = &["java", "kotlin", "scala", "groovy"];

/// A path cut into its module, source set and package directories.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parts {
    /// Directories before `src/<set>` (empty: none, or not a JVM layout).
    pub module: Vec<String>,
    /// `main`, `test`, `openapi`, `test/resources`...: `None` outside a
    /// `src/<set>/` layout.
    pub set: Option<String>,
    /// Holds code with packages (`src/<set>/java/...`).
    pub code: bool,
    /// Directories after the set (the package), or all the directories.
    pub dirs: Vec<String>,
    pub name: String,
}

pub fn split_path(path: &str) -> Parts {
    let segs: Vec<&str> = path.split('/').collect();
    let (dirs, name) = segs.split_at(segs.len() - 1);
    let name = name[0].to_string();
    let own = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    // `<module>/src/<set>/<lang>/<package...>/<file>`
    let src = dirs
        .iter()
        .enumerate()
        .rev()
        .find(|(i, d)| **d == "src" && i + 1 < dirs.len())
        .map(|(i, _)| i);
    if let Some(i) = src {
        let set = dirs[i + 1];
        let lang = dirs.get(i + 2).copied();
        let known_set = matches!(
            set,
            "main" | "test" | "it" | "integrationTest" | "testFixtures" | "androidTest"
        );
        if known_set {
            let code = lang.is_some_and(|l| CODE_DIRS.contains(&l));
            let (label, rest) = match lang {
                Some(_) if code => (set.to_string(), &dirs[i + 3..]),
                // `src/main/openapi` → `openapi`, `src/test/resources` → `test/resources`.
                Some(l) if set == "main" => (l.to_string(), &dirs[i + 3..]),
                Some(l) => (format!("{set}/{l}"), &dirs[i + 3..]),
                None => (set.to_string(), &dirs[i + 2..]),
            };
            return Parts {
                module: own(&dirs[..i]),
                set: Some(label),
                code,
                dirs: own(rest),
                name,
            };
        }
    }
    Parts {
        module: Vec::new(),
        set: None,
        code: false,
        dirs: own(dirs),
        name,
    }
}

/// A test file, by its place or its name.
pub fn is_test(path: &str) -> bool {
    let p = split_path(path);
    if p.set
        .as_deref()
        .is_some_and(|s| s.starts_with("test") || s == "it" || s.ends_with("Test"))
    {
        return true;
    }
    if p.dirs
        .iter()
        .chain(&p.module)
        .any(|d| matches!(d.as_str(), "test" | "tests" | "__tests__" | "e2e"))
    {
        return true;
    }
    let stem = stem(&p.name);
    p.name.contains(".spec.") || p.name.contains(".test.") || test_subject(stem).is_some()
}

/// Source code the explorer pairs with tests.
fn is_code(path: &str) -> bool {
    let ext = path.rsplit_once('.').map_or("", |(_, e)| e);
    matches!(
        ext,
        "java" | "kt" | "kts" | "scala" | "groovy" | "ts" | "tsx" | "js" | "jsx"
    )
}

/// `Foo.java` → `Foo`, `foo.spec.ts` → `foo`.
fn stem(name: &str) -> &str {
    let name = name.split(".spec.").next().unwrap_or(name);
    let name = name.split(".test.").next().unwrap_or(name);
    name.split_once('.').map_or(name, |(s, _)| s)
}

/// `FooTest`, `FooTests`, `FooIT`, `FooTestCase` → `Foo`.
fn test_subject(stem: &str) -> Option<&str> {
    ["Tests", "Test", "IT", "TestCase"]
        .iter()
        .find_map(|s| stem.strip_suffix(s))
        .filter(|s| !s.is_empty())
}

/// For each module: the package directories every code file of it shares.
fn package_roots(parts: &[Parts]) -> HashMap<Vec<String>, Vec<String>> {
    let mut roots: HashMap<Vec<String>, Vec<String>> = HashMap::new();
    for p in parts.iter().filter(|p| p.code) {
        // Keep at least one directory of the file's own.
        let dirs = &p.dirs[..p.dirs.len().saturating_sub(1)];
        match roots.get_mut(&p.module) {
            None => {
                roots.insert(p.module.clone(), dirs.to_vec());
            }
            Some(root) => {
                let n = root.iter().zip(dirs).take_while(|(a, b)| a == b).count();
                root.truncate(n);
            }
        }
    }
    roots
}

/// Directories of a file under its source set, the package root removed.
fn relative_dirs<'a>(p: &'a Parts, roots: &HashMap<Vec<String>, Vec<String>>) -> &'a [String] {
    match roots.get(&p.module) {
        Some(root) if p.code && p.dirs.starts_with(root) => &p.dirs[root.len()..],
        _ => &p.dirs,
    }
}

// ----- tree -----------------------------------------------------------------

#[derive(Debug, Clone)]
enum Child {
    Dir(Node),
    /// A file, with the directories merged into its line.
    File(usize, String),
}

#[derive(Debug, Clone, Default)]
struct Node {
    key: String,
    label: String,
    note: String,
    /// A source set: never merged with its parent or its children.
    set: bool,
    children: Vec<Child>,
}

impl Node {
    fn dir(&mut self, key: &str, label: &str, set: bool) -> &mut Node {
        let i = self
            .children
            .iter()
            .position(|c| matches!(c, Child::Dir(d) if d.label == label && d.set == set));
        let i = match i {
            Some(i) => i,
            None => {
                self.children.push(Child::Dir(Node {
                    key: key.to_string(),
                    label: label.to_string(),
                    set,
                    ..Node::default()
                }));
                self.children.len() - 1
            }
        };
        match &mut self.children[i] {
            Child::Dir(d) => d,
            Child::File(..) => unreachable!("a directory"),
        }
    }

    fn files(&self, out: &mut Vec<usize>) {
        for c in &self.children {
            match c {
                Child::Dir(d) => d.files(out),
                Child::File(f, _) => out.push(*f),
            }
        }
    }

    /// Merges chains of single directories, and single files into their line.
    fn compact(&mut self) {
        for c in &mut self.children {
            let Child::Dir(d) = c else {
                continue;
            };
            d.compact();
            while !d.set
                && d.children.len() == 1
                && matches!(&d.children[0], Child::Dir(x) if !x.set)
            {
                let Some(Child::Dir(x)) = d.children.pop() else {
                    unreachable!("checked above");
                };
                d.label = format!("{}/{}", d.label, x.label);
                d.key = x.key;
                d.children = x.children;
            }
            let hoist = match d.children.as_slice() {
                [Child::File(f, prefix)] if !d.set => {
                    Some(Child::File(*f, format!("{}/{prefix}", d.label)))
                }
                _ => None,
            };
            if let Some(h) = hoist {
                *c = h;
            }
        }
    }

    fn flatten(&self, depth: u16, folded: &HashSet<String>, out: &mut Vec<SideItem>) {
        for c in &self.children {
            match c {
                Child::Dir(d) => {
                    let mut files = Vec::new();
                    d.files(&mut files);
                    out.push(SideItem::Dir {
                        key: d.key.clone(),
                        depth,
                        label: d.label.clone(),
                        note: d.note.clone(),
                        files,
                    });
                    if !folded.contains(&d.key) {
                        d.flatten(depth + 1, folded, out);
                    }
                }
                Child::File(f, prefix) => out.push(SideItem::File {
                    file: *f,
                    depth,
                    prefix: prefix.clone(),
                    role: Role::Plain,
                    place: String::new(),
                }),
            }
        }
    }
}

/// The compact tree of `files` (index, path), folded directories closed.
pub fn tree(files: &[(usize, &str)], folded: &HashSet<String>) -> Vec<SideItem> {
    let parts: Vec<Parts> = files.iter().map(|(_, p)| split_path(p)).collect();
    let roots = package_roots(&parts);
    let mut root = Node::default();
    // Modules whose package root was shown already.
    let mut shown: HashSet<Vec<String>> = HashSet::new();
    for ((f, _), p) in files.iter().zip(&parts) {
        let mut node = &mut root;
        let mut key = String::new();
        for m in &p.module {
            key.push_str(m);
            key.push('/');
            node = node.dir(&key, m, false);
        }
        if let Some(set) = &p.set {
            key.push_str(&format!("[{set}]/"));
            node = node.dir(&key, set, true);
            if p.code && node.note.is_empty() {
                let pkg = roots.get(&p.module).filter(|r| !r.is_empty());
                if let Some(r) = pkg {
                    node.note = if shown.insert(p.module.clone()) {
                        r.join(".")
                    } else {
                        "same package".into()
                    };
                }
            }
        }
        for d in relative_dirs(p, &roots) {
            key.push_str(d);
            key.push('/');
            node = node.dir(&key, d, false);
        }
        node.children.push(Child::File(*f, String::new()));
    }
    root.compact();
    let mut out = Vec::new();
    root.flatten(0, folded, &mut out);
    out
}

/// Keys of every directory of the tree (for `zM`).
pub fn dir_keys(files: &[(usize, &str)]) -> HashSet<String> {
    tree(files, &HashSet::new())
        .into_iter()
        .filter_map(|it| match it {
            SideItem::Dir { key, .. } => Some(key),
            SideItem::File { .. } => None,
        })
        .collect()
}

// ----- pairs and flat -------------------------------------------------------

/// Where a file lives, its package root removed: `main · habilitation/dto`.
fn place(p: &Parts, roots: &HashMap<Vec<String>, Vec<String>>) -> String {
    let dirs = relative_dirs(p, roots).join("/");
    match &p.set {
        Some(set) if dirs.is_empty() => set.clone(),
        Some(set) => format!("{set} · {dirs}"),
        None => dirs,
    }
}

/// Each class with its tests beneath, by directory; `graph_subject` gives
/// the class a test file exercises, from the code graph.
pub fn pairs(
    files: &[(usize, &str)],
    graph_subject: impl Fn(usize) -> Option<usize>,
) -> Vec<SideItem> {
    let parts: Vec<Parts> = files.iter().map(|(_, p)| split_path(p)).collect();
    let roots = package_roots(&parts);
    let classes: Vec<usize> = (0..files.len())
        .filter(|&i| is_code(files[i].1) && !is_test(files[i].1))
        .collect();
    let rel = |i: usize| relative_dirs(&parts[i], &roots).to_vec();
    // Test (position in `files`) → class (position), and how.
    let mut subject: HashMap<usize, (usize, bool)> = HashMap::new();
    for t in (0..files.len()).filter(|&i| is_test(files[i].1)) {
        let tstem = stem(&parts[t].name);
        let exact = test_subject(tstem).unwrap_or(tstem);
        let same_dir = |c: &usize| rel(*c) == rel(t);
        let by_name: Vec<usize> = classes
            .iter()
            .copied()
            .filter(|&c| stem(&parts[c].name) == exact)
            .collect();
        let found = by_name
            .iter()
            .copied()
            .find(same_dir)
            .or(by_name.first().copied())
            .or_else(|| {
                // `FooConcurrencyIT` → the longest class name it starts with.
                classes
                    .iter()
                    .copied()
                    .filter(|&c| {
                        let s = stem(&parts[c].name);
                        s.len() >= 3 && tstem.len() > s.len() && tstem.starts_with(s)
                    })
                    .max_by_key(|&c| (stem(&parts[c].name).len(), same_dir(&c)))
            });
        match found {
            Some(c) => {
                subject.insert(t, (c, false));
            }
            None => {
                let by_graph = graph_subject(files[t].0)
                    .and_then(|f| files.iter().position(|(x, _)| *x == f))
                    .filter(|c| classes.contains(c));
                if let Some(c) = by_graph {
                    subject.insert(t, (c, true));
                }
            }
        }
    }
    let mut tests_of: HashMap<usize, Vec<(usize, bool)>> = HashMap::new();
    for (&t, &(c, g)) in &subject {
        tests_of.entry(c).or_default().push((t, g));
    }
    for v in tests_of.values_mut() {
        v.sort_unstable();
    }
    // Directories in order of appearance, paired tests left out.
    let mut groups: Vec<(String, Vec<usize>)> = Vec::new();
    for i in (0..files.len()).filter(|i| !subject.contains_key(i)) {
        let p = &parts[i];
        let mut dir = rel(i).join("/");
        if !p.code {
            dir = place(p, &roots);
        }
        match groups.iter_mut().find(|(d, _)| *d == dir) {
            Some((_, v)) => v.push(i),
            None => groups.push((dir, vec![i])),
        }
    }
    let mut out = Vec::new();
    for (dir, items) in groups {
        let mut all = Vec::new();
        for &i in &items {
            all.push(files[i].0);
            all.extend(
                tests_of
                    .get(&i)
                    .into_iter()
                    .flatten()
                    .map(|(t, _)| files[*t].0),
            );
        }
        out.push(SideItem::Dir {
            key: format!("pairs:{dir}"),
            depth: 0,
            label: if dir.is_empty() { "./".into() } else { dir },
            note: String::new(),
            files: all,
        });
        for i in items {
            let tests = tests_of.get(&i).cloned().unwrap_or_default();
            let role = if classes.contains(&i) {
                Role::Class {
                    tested: !tests.is_empty(),
                }
            } else {
                Role::Plain
            };
            out.push(SideItem::File {
                file: files[i].0,
                depth: 1,
                prefix: String::new(),
                role,
                place: String::new(),
            });
            for (t, by_graph) in tests {
                out.push(SideItem::File {
                    file: files[t].0,
                    depth: 2,
                    prefix: String::new(),
                    role: Role::Test { by_graph },
                    place: String::new(),
                });
            }
        }
    }
    out
}

/// One line per file, in the order of the diff.
pub fn flat(files: &[(usize, &str)]) -> Vec<SideItem> {
    let parts: Vec<Parts> = files.iter().map(|(_, p)| split_path(p)).collect();
    let roots = package_roots(&parts);
    files
        .iter()
        .zip(&parts)
        .map(|((f, _), p)| SideItem::File {
            file: *f,
            depth: 0,
            prefix: String::new(),
            role: Role::Plain,
            place: place(p, &roots),
        })
        .collect()
}

/// Removes the pairs-mode folds of a list keyed by directory: every
/// directory folded in `folded` hides its items.
pub fn fold_flat(items: Vec<SideItem>, folded: &HashSet<String>) -> Vec<SideItem> {
    let mut out = Vec::with_capacity(items.len());
    let mut hide_below: Option<u16> = None;
    for it in items {
        if let Some(d) = hide_below {
            if it.depth() > d {
                continue;
            }
            hide_below = None;
        }
        if let SideItem::Dir { key, depth, .. } = &it
            && folded.contains(key)
        {
            hide_below = Some(*depth);
        }
        out.push(it);
    }
    out
}

#[cfg(test)]
mod tests;
