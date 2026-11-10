//! Map mode of the Graph view: one box at a time, at one level of the graph
//! (modules, the files of a module, the symbols of a file). The selection is
//! in the middle, what uses it on the left, what it uses on the right.
//!
//! `Enter` on a neighbour puts it in the middle (`Backspace` goes back to the
//! previous one), on the middle box goes down a level (a module shows its
//! files, a file its symbols, a symbol opens its tree). A neighbour outside
//! the level (another module, another file) is followed there.

use std::collections::BTreeMap;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use survol_core::graph::level::{self, Level, LevelEdge, LevelNode, NodeRef, Scope};
use survol_core::graph::{EdgeKind, Graph, SymIdx};

use super::Scroll;
use super::export::Diagram;

/// Middle boxes remembered at one level, and places entered: past it, the
/// oldest are forgotten.
const MAX_HISTORY: usize = 100;

/// What a key asks the Graph view to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    None,
    /// Show the tree of this symbol.
    Symbol(SymIdx),
    /// Not a key of the map.
    Ignored,
}

/// The three columns of the map.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Column {
    /// What uses the middle box.
    Left,
    #[default]
    Center,
    /// What the middle box uses.
    Right,
}

/// A box of a side column: a node and its link with the middle box.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Neighbour {
    /// Index into [`Level::nodes`].
    pub node: usize,
    /// Index into [`Level::edges`].
    pub edge: usize,
}

/// A place visited: a level, its middle box, and the boxes put in the middle
/// before it at that level.
#[derive(Debug, Clone)]
struct Place {
    scope: Scope,
    /// `None`: the default box of the level.
    center: Option<NodeRef>,
    history: Vec<NodeRef>,
}

impl Place {
    fn new(scope: Scope, center: Option<NodeRef>) -> Self {
        Place {
            scope,
            center,
            history: Vec::new(),
        }
    }
}

/// Where the map is, carried over a reload (`Ctrl-r`): the places entered
/// with their history, and the selected box.
#[derive(Debug, Clone)]
pub struct Memo {
    places: Vec<Place>,
    selected: Option<(Column, NodeRef)>,
}

#[derive(Default)]
pub struct MapState {
    /// The places entered, the shown one last; empty until first shown.
    places: Vec<Place>,
    /// Restored from a [`Memo`]: the level is still to be built, with this
    /// box selected.
    pending: Option<Option<(Column, NodeRef)>>,
    pub level: Level,
    /// The short name of each node, and the path it lives in (see [`Name`]).
    pub names: Vec<Name>,
    /// Written on the border of each box: the number of changed symbols
    /// inside, `M` / `D` for a modified / removed symbol.
    pub badges: Vec<String>,
    /// The middle box, `None` when the level is empty.
    pub center: Option<usize>,
    pub left: Vec<Neighbour>,
    pub right: Vec<Neighbour>,
    pub column: Column,
    /// Cursor and scroll of the left and right columns, in boxes.
    pub left_pos: Scroll,
    pub right_pos: Scroll,
    /// Unused: the map has no list.
    pub pos: Scroll,
}

impl MapState {
    pub fn scope(&self) -> &Scope {
        self.places.last().map_or(&Scope::Root, |p| &p.scope)
    }

    /// Builds the map when it is first shown, or restored.
    pub fn ensure(&mut self, g: &Graph) {
        if self.places.is_empty() {
            self.push(Place::new(Scope::Root, None));
            self.show(g);
        } else if self.pending.is_some() {
            self.refresh(g);
        }
    }

    /// Where the map is, `None` before it is first shown.
    pub fn memo(&self) -> Option<Memo> {
        let selected = self.selected_node().map(|n| (self.column, n.of.clone()));
        (!self.places.is_empty()).then(|| Memo {
            places: self.places.clone(),
            selected,
        })
    }

    /// Back where `memo` was, once the graph is there.
    pub fn restore(&mut self, memo: Memo) {
        self.places = memo.places;
        self.pending = Some(memo.selected);
    }

    /// The graph changed: the same place, rebuilt, the same box selected
    /// when it is still there.
    pub fn refresh(&mut self, g: &Graph) {
        if self.places.is_empty() {
            return;
        }
        let selected = match self.pending.take() {
            Some(selected) => selected,
            None => self.selected_node().map(|n| (self.column, n.of.clone())),
        };
        self.show(g);
        let Some((column, of)) = selected else {
            return;
        };
        let side = match column {
            Column::Left => &self.left,
            Column::Right => &self.right,
            Column::Center => return,
        };
        let at = side.iter().position(|n| self.level.nodes[n.node].of == of);
        if let Some(i) = at {
            self.column = column;
            self.pos_mut(column).cursor = i;
        }
    }

    /// Builds the level of the current place, its middle box on the place's
    /// center (else the first box of the level: the most changed one).
    fn show(&mut self, g: &Graph) {
        let Some(place) = self.places.last_mut() else {
            return;
        };
        self.level = level::level(g, &place.scope);
        self.names = names(&self.level.nodes);
        self.badges = self.level.nodes.iter().map(badge).collect();
        let at = |of: &NodeRef| self.level.nodes.iter().position(|n| n.of == *of);
        let center = place.center.as_ref().and_then(at).or_else(|| {
            // The first box: the most changed one.
            (!self.level.nodes.is_empty()).then_some(0)
        });
        // The graph may have changed: forget the boxes no longer there.
        place.history.retain(|h| at(h).is_some());
        place.center = center.map(|i| self.level.nodes[i].of.clone());
        self.set_center(center);
    }

    /// Puts box `center` of the level in the middle, the cursor on it.
    fn set_center(&mut self, center: Option<usize>) {
        self.center = center;
        (self.left, self.right) = match center {
            Some(c) => columns(&self.level, c),
            None => Default::default(),
        };
        self.column = Column::Center;
        self.left_pos = Scroll::default();
        self.right_pos = Scroll::default();
    }

    /// Puts another box of the level in the middle, remembering this one.
    fn recenter(&mut self, node: usize) {
        if Some(node) == self.center {
            return;
        }
        let of = self.level.nodes[node].of.clone();
        if let Some(place) = self.places.last_mut()
            && let Some(old) = place.center.replace(of)
        {
            place.history.push(old);
            if place.history.len() > MAX_HISTORY {
                place.history.remove(0);
            }
        }
        self.set_center(Some(node));
    }

    /// Enters `place`, forgetting the oldest places past the maximum.
    fn push(&mut self, place: Place) {
        self.places.push(place);
        if self.places.len() > MAX_HISTORY {
            self.places.remove(0);
        }
    }

    /// The middle box and its columns as a Mermaid flowchart (`y` / `gx`).
    pub fn diagram(&self) -> Option<Diagram> {
        let center = self.center?;
        let label = |i: usize| {
            let badge = &self.badges[i];
            let name = self.names[i].name.replace('"', "#quot;");
            match badge.as_str() {
                "" => name,
                b => format!("{name} · {b}"),
            }
        };
        let mut out = String::from("flowchart LR\n");
        out.push_str(&format!("  c[\"{}\"]\n", label(center)));
        let mut outside: Vec<String> = Vec::new();
        let mut changed: Vec<String> = vec!["c".into()];
        for (prefix, side) in [("l", &self.left), ("r", &self.right)] {
            for (k, n) in side.iter().enumerate() {
                let id = format!("{prefix}{k}");
                let node = &self.level.nodes[n.node];
                out.push_str(&format!("  {id}[\"{}\"]\n", label(n.node)));
                if node.outside {
                    outside.push(id.clone());
                } else if node.changed() {
                    changed.push(id.clone());
                }
                let e = self.edge(*n);
                let arrow = if e.count == 0 { "-.->" } else { "-->" };
                let text = link_labels(e).into_iter().next().unwrap_or_default();
                let (a, b) = if prefix == "l" {
                    (id.as_str(), "c")
                } else {
                    ("c", id.as_str())
                };
                out.push_str(&format!("  {a} {arrow}|\"{text}\"| {b}\n"));
            }
        }
        out.push_str("  classDef changed font-weight:bold\n");
        out.push_str("  classDef outside stroke-dasharray:4 4,color:#888\n");
        out.push_str(&format!("  class {} changed\n", changed.join(",")));
        if !outside.is_empty() {
            out.push_str(&format!("  class {} outside\n", outside.join(",")));
        }
        let mut path = self.breadcrumb();
        path.push(self.names[center].name.clone());
        Some(Diagram {
            title: format!("Map: {}", path.join(" › ")),
            mermaid: out,
        })
    }

    pub fn center_node(&self) -> Option<&LevelNode> {
        self.level.nodes.get(self.center?)
    }

    fn pos_mut(&mut self, column: Column) -> &mut Scroll {
        match column {
            Column::Left => &mut self.left_pos,
            _ => &mut self.right_pos,
        }
    }

    /// The selected box of a side column.
    pub fn selected_neighbour(&self) -> Option<Neighbour> {
        match self.column {
            Column::Left => self.left.get(self.left_pos.cursor).copied(),
            Column::Right => self.right.get(self.right_pos.cursor).copied(),
            Column::Center => None,
        }
    }

    /// The node of the selected box, in any column.
    pub fn selected_node(&self) -> Option<&LevelNode> {
        match self.selected_neighbour() {
            Some(n) => self.level.nodes.get(n.node),
            None => self.center_node(),
        }
    }

    /// The link of a side box with the middle one.
    pub fn edge(&self, n: Neighbour) -> &LevelEdge {
        &self.level.edges[n.edge]
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

    /// The changed boxes of the level, and which of them is in the middle.
    pub fn changed_position(&self) -> (Option<usize>, usize) {
        let changed: Vec<usize> = changed_nodes(&self.level).collect();
        let at = self
            .center
            .and_then(|c| changed.iter().position(|&i| i == c));
        (at, changed.len())
    }

    /// `Enter`: down a level from the middle box; a side box to the middle,
    /// or followed to its own level when it lies outside this one.
    fn enter(&mut self, g: &Graph) -> Outcome {
        let Some(node) = self.selected_node().cloned() else {
            return Outcome::None;
        };
        let place = match self.selected_neighbour() {
            None => match &node.of {
                NodeRef::Module(m) => Place::new(Scope::Module(m.clone()), None),
                NodeRef::File(path) => Place::new(Scope::File(path.clone()), None),
                NodeRef::Symbol(s) => return Outcome::Symbol(*s),
            },
            Some(n) if !node.outside => {
                self.recenter(n.node);
                return Outcome::None;
            }
            Some(_) => {
                let from = self.center_node().map(|c| c.of.clone());
                match from.and_then(|from| level::follow(g, &from, &node.of)) {
                    Some(to) => Place::new(level::home(g, &to), Some(to)),
                    None => match &node.of {
                        NodeRef::Module(m) => Place::new(Scope::Module(m.clone()), None),
                        NodeRef::File(path) => Place::new(Scope::File(path.clone()), None),
                        NodeRef::Symbol(s) => {
                            Place::new(level::home(g, &node.of), Some(NodeRef::Symbol(*s)))
                        }
                    },
                }
            }
        };
        self.push(place);
        self.show(g);
        Outcome::None
    }

    /// `Backspace`: the previous middle box of this level, else the place
    /// before this one, else up a level. `false` at the root with nothing
    /// before.
    pub fn back(&mut self, g: &Graph) -> bool {
        let Some(place) = self.places.last_mut() else {
            return false;
        };
        if let Some(previous) = place.history.pop() {
            place.center = Some(previous.clone());
            let at = self.level.nodes.iter().position(|n| n.of == previous);
            self.set_center(at);
            return true;
        }
        if self.places.len() > 1 {
            self.places.pop();
            self.show(g);
            return true;
        }
        let scope = self.scope().clone();
        let Some(parent) = level::parent(g, &scope) else {
            return false;
        };
        let from = match scope {
            Scope::Module(m) => Some(NodeRef::Module(m)),
            Scope::File(path) => Some(NodeRef::File(path)),
            Scope::Root => None,
        };
        self.places = vec![Place::new(parent, from)];
        self.show(g);
        true
    }

    /// `h` / `l`: the column on the left / right, when it has boxes.
    fn step(&mut self, right: bool) {
        self.column = match (self.column, right) {
            (Column::Center, false) if !self.left.is_empty() => Column::Left,
            (Column::Center, true) if !self.right.is_empty() => Column::Right,
            (Column::Left, true) | (Column::Right, false) => Column::Center,
            (column, _) => column,
        };
    }

    /// `j` / `k`: the next / previous box of the column.
    fn move_by(&mut self, delta: isize) {
        let (pos, len) = match self.column {
            Column::Left => (&mut self.left_pos, self.left.len()),
            Column::Right => (&mut self.right_pos, self.right.len()),
            Column::Center => return,
        };
        pos.cursor = pos
            .cursor
            .saturating_add_signed(delta)
            .min(len.saturating_sub(1));
        pos.clamp(len);
    }

    /// `n` / `N`: the next / previous changed box of the level to the middle.
    pub fn next_changed(&mut self, forward: bool) {
        if let Some(i) = next_changed(&self.level, self.center, forward) {
            self.recenter(i);
        }
    }

    pub fn on_key(&mut self, g: &Graph, key: KeyEvent) -> Outcome {
        let back = match key.code {
            KeyCode::Char('o') => key.modifiers.contains(KeyModifiers::CONTROL),
            KeyCode::Backspace | KeyCode::Char('u') | KeyCode::Char('-') => {
                !key.modifiers.contains(KeyModifiers::CONTROL)
            }
            _ => false,
        };
        if back {
            // At the root with nothing before: the Graph view's own back.
            let handled = self.back(g) || key.code == KeyCode::Char('u');
            return if handled {
                Outcome::None
            } else {
                Outcome::Ignored
            };
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            return Outcome::Ignored;
        }
        match key.code {
            KeyCode::Char('h') | KeyCode::Left => self.step(false),
            KeyCode::Char('l') | KeyCode::Right => self.step(true),
            KeyCode::Char('k') | KeyCode::Up => self.move_by(-1),
            KeyCode::Char('j') | KeyCode::Down => self.move_by(1),
            KeyCode::Char('n') => self.next_changed(true),
            KeyCode::Char('N') => self.next_changed(false),
            KeyCode::Enter | KeyCode::Char('i') => return self.enter(g),
            _ => return Outcome::Ignored,
        }
        Outcome::None
    }
}

/// The changed boxes inside the level, in the order of the level.
fn changed_nodes(level: &Level) -> impl Iterator<Item = usize> + '_ {
    (0..level.nodes.len()).filter(|&i| level.nodes[i].changed() && !level.nodes[i].outside)
}

/// The columns around box `center`: what uses it, what it uses. The boxes of
/// the level first, then those outside it; in each group the changed boxes
/// first, then the most linked, then by name.
fn columns(level: &Level, center: usize) -> (Vec<Neighbour>, Vec<Neighbour>) {
    let side = |incoming: bool| {
        let mut out: Vec<Neighbour> = level
            .edges
            .iter()
            .enumerate()
            .filter_map(|(edge, e)| {
                let (near, far) = if incoming {
                    (e.to, e.from)
                } else {
                    (e.from, e.to)
                };
                (near == center && far != center).then_some(Neighbour { node: far, edge })
            })
            .collect();
        out.sort_by_key(|n| {
            let node = &level.nodes[n.node];
            (
                node.outside,
                !node.changed(),
                std::cmp::Reverse(level.edges[n.edge].total()),
                node.label.clone(),
            )
        });
        out
    };
    (side(true), side(false))
}

/// The changed box after (`forward`) or before `center`, in the order of the
/// level, going round.
fn next_changed(level: &Level, center: Option<usize>, forward: bool) -> Option<usize> {
    let changed: Vec<usize> = changed_nodes(level).collect();
    let here = center.and_then(|c| changed.iter().position(|&i| i == c));
    let n = changed.len();
    let i = match (here, forward) {
        _ if n == 0 => return None,
        (Some(i), true) => (i + 1) % n,
        (Some(i), false) => (i + n - 1) % n,
        (None, true) => 0,
        (None, false) => n - 1,
    };
    Some(changed[i])
}

/// The full name of a kind of link, and a short one.
fn kind_names(k: EdgeKind) -> (&'static str, &'static str) {
    match k {
        EdgeKind::Calls => ("calls", "call"),
        EdgeKind::Imports => ("imports", "imp"),
        EdgeKind::Inherits => ("extends", "ext"),
        EdgeKind::Overrides => ("overrides", "ovr"),
        EdgeKind::Uses => ("uses", "use"),
        EdgeKind::Tests => ("tests", "test"),
        EdgeKind::Injects => ("injects", "inj"),
        EdgeKind::HttpCalls => ("http", "http"),
        EdgeKind::Configures => ("config", "cfg"),
        EdgeKind::Routes => ("routes", "route"),
        EdgeKind::Publishes => ("events", "evt"),
    }
}

/// The labels of a link, the longest first: its main kind (`+` when there
/// are others) and how many links it folds; `via` for links through an
/// interface. Each keeps at least a short name of the kind.
pub fn link_labels(e: &LevelEdge) -> Vec<String> {
    // Labels of one set of links, the longest first.
    let of = |kinds: &BTreeMap<EdgeKind, usize>| -> Vec<String> {
        let Some((k, _)) = kinds.iter().max_by_key(|(_, n)| **n) else {
            return Vec::new();
        };
        let (full, short) = kind_names(*k);
        let count: usize = kinds.values().sum();
        let more = if kinds.len() > 1 { "+" } else { "" };
        let n = if count > 1 {
            format!(" {count}")
        } else {
            String::new()
        };
        let mut out = vec![
            format!("{full}{more}{n}"),
            format!("{short}{more}{n}"),
            format!("{short}{n}"),
            short.to_string(),
        ];
        out.dedup();
        out
    };
    let direct = of(&e.kinds);
    let via = of(&e.via);
    if direct.is_empty() {
        return via
            .iter()
            .map(|l| format!("via {l}"))
            .chain(["via".into()])
            .collect();
    }
    if via.is_empty() {
        return direct;
    }
    // `+via` dropped before the number of links.
    let with_count = direct.iter().filter(|l| l.contains(' '));
    let both = with_count.map(|l| format!("{l} +via"));
    both.chain(direct.iter().cloned()).collect()
}

/// How a node is named on the map.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Name {
    /// In the box: the last parts of a module (as many as it takes to tell it
    /// from the other modules of the level), a file name, a symbol.
    pub name: String,
    /// Where it lives: the package or directory above. Empty for a symbol.
    pub parent: String,
}

/// The names of the nodes.
fn names(nodes: &[LevelNode]) -> Vec<Name> {
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
                while !parent.is_empty()
                    && modules.iter().any(|m| *m != full && ends_like(m, &name))
                {
                    name = full[split(&parent).0.len()..]
                        .trim_start_matches(['.', '/'])
                        .to_string();
                    parent = split(&parent).0;
                }
                Name { name, parent }
            }
            NodeRef::File(path) => Name {
                name: n.label.clone(),
                parent: path.rsplit_once('/').map_or("", |(dir, _)| dir).to_string(),
            },
            NodeRef::Symbol(_) => Name {
                name: n.label.clone(),
                parent: String::new(),
            },
        })
        .collect()
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
            test: false,
        }
    }

    fn edge(from: usize, to: usize, count: usize) -> LevelEdge {
        LevelEdge {
            from,
            to,
            count,
            kinds: BTreeMap::from([(EdgeKind::Calls, count)]),
            via: BTreeMap::new(),
        }
    }

    /// `center` (changed) used by a, b (changed), c; using d, e (outside).
    fn star() -> Level {
        let mut nodes: Vec<LevelNode> = ["center", "a", "b", "c", "d", "e"]
            .iter()
            .map(|n| module(n))
            .collect();
        nodes[0].changes = 2;
        nodes[2].changes = 1;
        nodes[5].changes = 4;
        nodes[5].outside = true;
        let edges = vec![
            edge(1, 0, 5),
            edge(2, 0, 1),
            edge(3, 0, 5),
            edge(0, 4, 2),
            edge(0, 5, 7),
            // Between neighbours: not in the columns.
            edge(1, 3, 9),
        ];
        Level { nodes, edges }
    }

    fn labels(level: &Level, side: &[Neighbour]) -> Vec<String> {
        side.iter()
            .map(|n| level.nodes[n.node].label.clone())
            .collect()
    }

    #[test]
    fn columns_hold_the_users_and_the_used_changed_first_then_by_links() {
        let level = star();
        let (left, right) = columns(&level, 0);
        // b is changed; a and c have as many links, by name.
        assert_eq!(labels(&level, &left), ["b", "a", "c"]);
        // e is changed, with fewer links, but outside the level: after d.
        assert_eq!(labels(&level, &right), ["d", "e"]);
        assert_eq!(level.edges[right[0].edge].count, 2);

        // Around a: what it uses, the middle box and c.
        let (left, right) = columns(&level, 1);
        assert!(left.is_empty());
        assert_eq!(labels(&level, &right), ["center", "c"]);
    }

    #[test]
    fn changed_boxes_follow_each_other_round_the_level() {
        let level = star();
        assert_eq!(next_changed(&level, Some(0), true), Some(2));
        assert_eq!(next_changed(&level, Some(2), true), Some(0));
        assert_eq!(next_changed(&level, Some(0), false), Some(2));
        // From an unchanged box: the first / last changed one.
        assert_eq!(next_changed(&level, Some(1), true), Some(0));
        assert_eq!(next_changed(&level, Some(1), false), Some(2));
        assert_eq!(next_changed(&Level::default(), None, true), None);
    }

    #[test]
    fn keys_move_between_columns_and_within_them() {
        let mut map = MapState {
            level: star(),
            ..MapState::default()
        };
        map.set_center(Some(0));
        let name = |map: &MapState| map.selected_node().unwrap().label.clone();
        assert_eq!(name(&map), "center");
        map.step(false);
        assert_eq!((map.column, name(&map)), (Column::Left, "b".into()));
        map.move_by(1);
        map.move_by(1);
        map.move_by(1);
        assert_eq!(name(&map), "c", "the cursor stops on the last box");
        map.step(false);
        assert_eq!(map.column, Column::Left, "nothing further left");
        map.step(true);
        map.step(true);
        assert_eq!((map.column, name(&map)), (Column::Right, "d".into()));

        // To the middle; back to the previous middle box.
        map.places
            .push(Place::new(Scope::Root, Some(map.level.nodes[0].of.clone())));
        map.recenter(4);
        assert_eq!((map.column, name(&map)), (Column::Center, "d".into()));
        assert_eq!(labels(&map.level, &map.left), ["center"]);
        map.next_changed(true);
        assert_eq!(name(&map), "center");
        let place = map.places.last().unwrap();
        assert_eq!(place.history.len(), 2);
    }

    #[test]
    fn link_labels_keep_a_short_kind_and_tell_links_through_an_interface() {
        let mut e = edge(0, 1, 1);
        assert_eq!(link_labels(&e), ["calls", "call"]);
        e.kinds.insert(EdgeKind::Injects, 43);
        e.count = 44;
        assert_eq!(link_labels(&e), ["injects+ 44", "inj+ 44", "inj 44", "inj"]);
        // Only through an interface.
        let mut via = edge(0, 1, 0);
        via.kinds.clear();
        via.via.insert(EdgeKind::Calls, 3);
        assert_eq!(
            link_labels(&via),
            ["via calls 3", "via call 3", "via call", "via"]
        );
        // Both: `+via` dropped before the number of links.
        e.via.insert(EdgeKind::Calls, 2);
        assert_eq!(
            link_labels(&e),
            [
                "injects+ 44 +via",
                "inj+ 44 +via",
                "inj 44 +via",
                "injects+ 44",
                "inj+ 44",
                "inj 44",
                "inj"
            ]
        );
    }

    #[test]
    fn the_history_of_a_level_is_bounded() {
        let mut map = MapState {
            level: star(),
            ..MapState::default()
        };
        map.places.push(Place::new(Scope::Root, None));
        map.set_center(Some(0));
        map.places[0].center = Some(map.level.nodes[0].of.clone());
        for k in 0..MAX_HISTORY + 20 {
            map.recenter(1 + k % 2);
        }
        let history = &map.places[0].history;
        assert_eq!(history.len(), MAX_HISTORY);
        // The newest kept: the box before the middle one.
        assert_eq!(*history.last().unwrap(), map.level.nodes[1].of);
    }

    #[test]
    fn the_diagram_draws_the_middle_box_and_its_columns() {
        let level = star();
        let mut map = MapState {
            names: names(&level.nodes),
            badges: level.nodes.iter().map(badge).collect(),
            level,
            ..MapState::default()
        };
        map.set_center(Some(0));
        let d = map.diagram().unwrap();
        assert_eq!(d.title, "Map: map › center");
        let m = &d.mermaid;
        assert!(m.starts_with("flowchart LR\n  c[\"center · 2\"]\n"), "{m}");
        // Users point to the middle box, the middle box to what it uses.
        assert!(
            m.contains("  l0[\"b · 1\"]\n  l0 -->|\"calls\"| c\n"),
            "{m}"
        );
        assert!(m.contains("  c -->|\"calls 2\"| r0\n"), "{m}");
        assert!(m.contains("class r1 outside"), "{m}");
    }

    #[test]
    fn modules_are_named_by_as_many_last_parts_as_they_need() {
        let nodes = [
            module("org.acme.app"),
            module("org.acme.app.owner"),
            module("org.acme.app.vet"),
            module("src/app/owner"),
        ];
        let named = names(&nodes);
        let plain: Vec<&str> = named.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(plain, ["app", "app.owner", "vet", "app/owner"]);
        assert_eq!(named[2].parent, "org.acme.app");
    }

    #[test]
    fn changes_go_to_the_badge() {
        let mut m = module("a");
        assert_eq!(badge(&m), "");
        m.changes = 342;
        assert_eq!(badge(&m), "342");
        m.outside = true;
        assert_eq!(badge(&m), "");
    }
}
