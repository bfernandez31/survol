//! Map mode of the Graph view: the code graph drawn as boxes and arrows,
//! one level at a time. `Enter` goes into a box (a module shows its files, a
//! file its symbols), `Backspace` back out; what is outside the level stays
//! on its border.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use survol_core::graph::level::{self, Level, LevelNode, NodeRef, Scope};
use survol_core::graph::{Graph, SymIdx};
use unicode_width::UnicodeWidthStr;

use super::boxes::{self, Direction, Drawing};

/// Nodes drawn at one level: what a screen or two can hold.
pub const MAX_NODES: usize = 36;
/// Links a level can draw before it becomes unreadable. Over it, the links
/// with the outside are left out, then the ones a path already implies.
pub const MAX_LINKS: usize = 20;
/// Longest label in a box.
const MAX_LABEL: usize = 30;

/// What a key asks the Graph view to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    None,
    /// Show the tree of this symbol.
    Symbol(SymIdx),
    /// Not a key of the map.
    Ignored,
}

#[derive(Default)]
pub struct MapState {
    /// `None` until the map is first shown.
    scope: Option<Scope>,
    /// `t`: every node of the level, not only around the changes.
    pub all: bool,
    pub level: Level,
    /// The short name of each node, and the path it lives in (see [`Name`]).
    pub names: Vec<Name>,
    /// The text of each box: the name, cut when too long.
    pub labels: Vec<String>,
    /// Written on the border of each box: the number of changed symbols
    /// inside, `M` / `D` for a modified / removed symbol.
    pub badges: Vec<String>,
    /// The frame of each node, when the level is drawn in frames (modules,
    /// by parent package).
    pub groups: Vec<usize>,
    pub drawing: Drawing,
    /// Links of the level drawn as arrows; the others only show in the
    /// details and as the colour of the boxes around the selected one.
    pub drawn: usize,
    pub selected: usize,
    /// Top-left cell of the drawing shown, kept by the renderer.
    pub scroll: (u16, u16),
    /// Unused: the map has no list.
    pub pos: super::Scroll,
}

impl MapState {
    pub fn scope(&self) -> &Scope {
        self.scope.as_ref().unwrap_or(&Scope::Root)
    }

    /// Builds the map when it is first shown or the graph changed.
    pub fn ensure(&mut self, g: &Graph) {
        if self.scope.is_none() {
            self.open(g, Scope::Root, None);
        }
    }

    /// The graph changed: the same place, rebuilt.
    pub fn refresh(&mut self, g: &Graph) {
        if let Some(scope) = self.scope.take() {
            let selected = self.selected_node().map(|n| n.of.clone());
            self.open(g, scope, selected.as_ref());
        }
    }

    /// Shows `scope`, the cursor on `select` when it is there, else on the
    /// first changed node.
    fn open(&mut self, g: &Graph, scope: Scope, select: Option<&NodeRef>) {
        self.level = level::level(g, &scope, self.all, MAX_NODES);
        let grouped = scope == Scope::Root;
        self.names = names(&self.level.nodes, grouped);
        self.labels = self.names.iter().map(|n| cut(&n.name)).collect();
        self.badges = self.level.nodes.iter().map(badge).collect();
        let widths: Vec<u16> = self
            .labels
            .iter()
            .zip(&self.badges)
            .map(|(l, b)| (l.width() as u16 + 4).max(b.width() as u16 + 6))
            .collect();
        let outside = |i: usize| self.level.nodes[i].outside;
        // Heaviest first, as the level gives them.
        let mut edges: Vec<(usize, usize)> =
            self.level.edges.iter().map(|e| (e.from, e.to)).collect();
        if edges.len() > MAX_LINKS {
            edges.retain(|&(a, b)| !outside(a) && !outside(b));
        }
        if edges.len() > MAX_LINKS {
            edges = reduce(self.level.nodes.len(), edges);
        }
        self.drawn = edges.len();
        self.groups.clear();
        self.drawing = if grouped {
            // One frame per parent, in the order of the nodes.
            let mut titles: Vec<String> = Vec::new();
            for name in &self.names {
                let g = titles.iter().position(|t| *t == name.frame);
                self.groups.push(g.unwrap_or(titles.len()));
                if g.is_none() {
                    titles.push(name.frame.clone());
                }
            }
            boxes::layout_grouped(&widths, &edges, &self.groups, &titles)
        } else {
            boxes::layout(&widths, &edges)
        };
        self.selected = select
            .and_then(|of| self.level.nodes.iter().position(|n| n.of == *of))
            .unwrap_or(0);
        self.scroll = (0, 0);
        self.scope = Some(scope);
    }

    pub fn selected_node(&self) -> Option<&LevelNode> {
        self.level.nodes.get(self.selected)
    }

    /// The names from the root to the scope.
    pub fn breadcrumb(&self) -> Vec<String> {
        let mut out = vec!["map".to_string()];
        match self.scope() {
            Scope::Root => {}
            Scope::Module(m) => out.push(m.clone()),
            Scope::File(path) => {
                let (dir, name) = path.rsplit_once('/').unwrap_or(("", path));
                out.push(dir.to_string());
                out.push(name.to_string());
            }
        }
        out
    }

    /// Goes into the selected box.
    fn enter(&mut self, g: &Graph) -> Outcome {
        let Some(node) = self.selected_node() else {
            return Outcome::None;
        };
        let scope = match &node.of {
            NodeRef::Module(m) => Scope::Module(m.clone()),
            NodeRef::File(path) => Scope::File(path.clone()),
            NodeRef::Symbol(s) => return Outcome::Symbol(*s),
        };
        self.open(g, scope, None);
        Outcome::None
    }

    /// Back to the level containing this one, the cursor on where we were.
    /// `false` at the root.
    pub fn leave(&mut self, g: &Graph) -> bool {
        let Some(parent) = level::parent(g, self.scope()) else {
            return false;
        };
        let from = match self.scope() {
            Scope::Module(m) => Some(NodeRef::Module(m.clone())),
            Scope::File(path) => Some(NodeRef::File(path.clone())),
            Scope::Root => None,
        };
        self.open(g, parent, from.as_ref());
        true
    }

    fn step(&mut self, direction: Direction) {
        if let Some(i) = boxes::step(&self.drawing.boxes, self.selected, direction) {
            self.selected = i;
        }
    }

    /// `n` / `N`: the next changed box, in reading order.
    fn next_changed(&mut self, forward: bool) {
        let mut order: Vec<usize> = (0..self.level.nodes.len())
            .filter(|&i| self.level.nodes[i].changed() && !self.level.nodes[i].outside)
            .collect();
        let key = |i: usize| (self.drawing.boxes[i].x, self.drawing.boxes[i].y);
        let Some(here) = self.drawing.boxes.get(self.selected).map(|b| (b.x, b.y)) else {
            return;
        };
        order.sort_by_key(|&i| key(i));
        let next = if forward {
            order.iter().find(|&&i| key(i) > here).or(order.first())
        } else {
            order
                .iter()
                .rev()
                .find(|&&i| key(i) < here)
                .or(order.last())
        };
        if let Some(&i) = next {
            self.selected = i;
        }
    }

    pub fn on_key(&mut self, g: &Graph, key: KeyEvent) -> Outcome {
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            return Outcome::Ignored;
        }
        match key.code {
            KeyCode::Char('h') | KeyCode::Left => self.step(Direction::Left),
            KeyCode::Char('l') | KeyCode::Right => self.step(Direction::Right),
            KeyCode::Char('k') | KeyCode::Up => self.step(Direction::Up),
            KeyCode::Char('j') | KeyCode::Down => self.step(Direction::Down),
            KeyCode::Char('n') => self.next_changed(true),
            KeyCode::Char('N') => self.next_changed(false),
            KeyCode::Enter | KeyCode::Char('i') => return self.enter(g),
            KeyCode::Char('u') | KeyCode::Char('-') => {
                self.leave(g);
            }
            KeyCode::Char('t') => {
                self.all = !self.all;
                let (scope, selected) = (
                    self.scope().clone(),
                    self.selected_node().map(|n| n.of.clone()),
                );
                self.open(g, scope, selected.as_ref());
            }
            _ => return Outcome::Ignored,
        }
        Outcome::None
    }
}

/// Drops the edges a longer path already implies (`a → c` when `a → b → c`),
/// the lightest first: what can be reached stays reachable.
fn reduce(nodes: usize, mut edges: Vec<(usize, usize)>) -> Vec<(usize, usize)> {
    let mut i = edges.len();
    while i > 0 {
        i -= 1;
        let (from, to) = edges[i];
        // Is `to` still reached from `from` without this edge?
        let mut seen = vec![false; nodes];
        let mut todo = vec![from];
        seen[from] = true;
        while let Some(v) = todo.pop() {
            for (k, &(a, b)) in edges.iter().enumerate() {
                if k != i && a == v && !seen[b] {
                    seen[b] = true;
                    todo.push(b);
                }
            }
        }
        if seen[to] {
            edges.remove(i);
        }
    }
    edges
}

/// How a node is named on the map.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Name {
    /// In the box: the last part of a module, a file name, a symbol.
    pub name: String,
    /// Where it lives: the package or directory above. Empty for a symbol.
    pub parent: String,
    /// The frame it is drawn in, when the level has frames: its parent, or
    /// itself when it is the parent of other modules.
    pub frame: String,
}

/// The names of the nodes. A module is named by its last part; without
/// frames (`framed` off), by as many last parts as it takes to tell it from
/// the other modules of the level.
fn names(nodes: &[LevelNode], framed: bool) -> Vec<Name> {
    let split = |s: &str| -> (String, String) {
        match s.rfind(['.', '/']) {
            Some(i) => (s[..i].to_string(), s[i + 1..].to_string()),
            None => (String::new(), s.to_string()),
        }
    };
    let modules: Vec<&str> = nodes
        .iter()
        .filter(|n| matches!(n.of, NodeRef::Module(_)))
        .map(|n| n.label.as_str())
        .collect();
    nodes
        .iter()
        .map(|n| match &n.of {
            NodeRef::Module(full) => {
                let (mut parent, mut name) = split(full);
                let ends_like = |other: &str, name: &str| {
                    other == name
                        || other.ends_with(&format!(".{name}"))
                        || other.ends_with(&format!("/{name}"))
                };
                while !framed
                    && !parent.is_empty()
                    && modules.iter().any(|m| *m != full && ends_like(m, &name))
                {
                    name = full[split(&parent).0.len()..]
                        .trim_start_matches(['.', '/'])
                        .to_string();
                    parent = split(&parent).0;
                }
                let has_children = modules.iter().any(|m| split(m).0 == *full);
                let frame = if has_children || parent.is_empty() {
                    full.clone()
                } else {
                    parent.clone()
                };
                Name {
                    name,
                    parent,
                    frame,
                }
            }
            NodeRef::File(path) => Name {
                name: n.label.clone(),
                parent: path.rsplit_once('/').map_or("", |(dir, _)| dir).to_string(),
                frame: String::new(),
            },
            NodeRef::Symbol(_) => Name {
                name: n.label.clone(),
                ..Name::default()
            },
        })
        .collect()
}

/// A name cut to [`MAX_LABEL`].
fn cut(name: &str) -> String {
    let chars: Vec<char> = name.chars().collect();
    if chars.len() <= MAX_LABEL {
        return name.to_string();
    }
    let head: String = chars[..MAX_LABEL - 1].iter().collect();
    format!("{head}…")
}

/// What changed in a node, for the border of its box.
fn badge(node: &LevelNode) -> String {
    match &node.of {
        _ if node.outside || !node.changed() => String::new(),
        NodeRef::Symbol(_) if node.removed => "D".into(),
        NodeRef::Symbol(_) => "M".into(),
        _ => node.changes.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn module(name: &str) -> LevelNode {
        LevelNode {
            of: NodeRef::Module(name.to_string()),
            label: name.to_string(),
            outside: false,
            changes: 0,
            removed: false,
            size: 1,
        }
    }

    #[test]
    fn implied_links_are_dropped_and_cycles_stay_connected() {
        assert_eq!(reduce(3, vec![(0, 1), (1, 2), (0, 2)]), [(0, 1), (1, 2)]);
        // 0 ↔ 1, both to 2: one of the two links to 2 stays.
        let kept = reduce(3, vec![(0, 1), (1, 0), (0, 2), (1, 2)]);
        assert_eq!(kept, [(0, 1), (1, 0), (0, 2)]);
    }

    #[test]
    fn modules_are_named_by_their_last_part_and_framed_by_parent() {
        let nodes = [
            module("org.acme.app"),
            module("org.acme.app.owner"),
            module("org.acme.app.vet"),
            module("src/app/owner"),
        ];
        let framed = names(&nodes, true);
        let name = |n: &Name| (n.name.clone(), n.frame.clone());
        assert_eq!(name(&framed[0]), ("app".into(), "org.acme.app".into()));
        assert_eq!(name(&framed[1]), ("owner".into(), "org.acme.app".into()));
        assert_eq!(name(&framed[3]), ("owner".into(), "src/app".into()));
        assert_eq!(framed[1].parent, "org.acme.app");

        // Without frames, two modules of the same name are told apart.
        let plain: Vec<String> = names(&nodes, false).into_iter().map(|n| n.name).collect();
        assert_eq!(plain, ["app", "app.owner", "vet", "app/owner"]);
    }

    #[test]
    fn long_names_are_cut_and_changes_go_to_the_badge() {
        let mut m = module("a");
        assert_eq!(badge(&m), "");
        m.changes = 342;
        assert_eq!(badge(&m), "342");
        m.outside = true;
        assert_eq!(badge(&m), "");
        let long = "x".repeat(40);
        assert_eq!(cut(&long).chars().count(), MAX_LABEL);
        assert!(cut(&long).ends_with('…'));
    }
}
