//! Parsing and validation of LLM answers.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::Deserialize;
use serde::de::DeserializeOwned;

use super::{Draft, Layer};

/// The JSON object in an answer, tolerating markdown fences and prose around it.
pub(super) fn extract_json(text: &str) -> &str {
    let t = text.trim();
    let t = match t.find("```").filter(|_| !t.starts_with('{')) {
        Some(start) => {
            let body = &t[start + 3..];
            let body = body.strip_prefix("json").unwrap_or(body);
            body.find("```").map_or(body, |end| &body[..end])
        }
        None => t,
    };
    match (t.find('{'), t.rfind('}')) {
        (Some(a), Some(b)) if a < b => &t[a..=b],
        _ => t.trim(),
    }
}

fn from_answer<T: DeserializeOwned>(text: &str) -> Result<T, String> {
    serde_json::from_str(extract_json(text)).map_err(|e| format!("invalid JSON: {e}"))
}

/// A hunk or group id: a number, tolerating `"12"`, `"h12"` or `"#12"`.
#[derive(Debug, Clone, Copy)]
struct Id(usize);

impl<'de> Deserialize<'de> for Id {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Num(usize),
            Str(String),
        }
        match Raw::deserialize(d)? {
            Raw::Num(n) => Ok(Id(n)),
            Raw::Str(s) => s
                .trim()
                .trim_start_matches(['h', 'H', '#', 'g', 'G'])
                .parse()
                .map(Id)
                .map_err(|_| serde::de::Error::custom(format!("`{s}` is not an id"))),
        }
    }
}

#[derive(Deserialize)]
struct RawGrouping {
    groups: Vec<RawGroup>,
}

#[derive(Deserialize)]
struct RawGroup {
    title: String,
    #[serde(default)]
    summary: String,
    layers: Vec<RawLayer>,
}

#[derive(Deserialize)]
struct RawLayer {
    name: String,
    hunks: Vec<Id>,
}

/// Parses a grouping answer. Ids are not checked here, see [`check`].
pub(super) fn groups(text: &str) -> Result<Vec<Draft>, String> {
    let raw: RawGrouping = from_answer(text)?;
    Ok(raw
        .groups
        .into_iter()
        .map(|g| Draft {
            title: g.title.trim().to_string(),
            summary: g.summary.trim().to_string(),
            layers: g
                .layers
                .into_iter()
                .map(|l| Layer {
                    name: layer_name(&l.name),
                    hunk_ids: l.hunks.into_iter().map(|i| i.0).collect(),
                })
                .collect(),
        })
        .collect())
}

fn layer_name(name: &str) -> String {
    let n = name.trim().to_lowercase();
    if n.is_empty() { "other".into() } else { n }
}

/// Invariant violations of a grouping against the expected hunk ids.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct Violations {
    pub unknown: BTreeSet<usize>,
    pub duplicate: BTreeSet<usize>,
    pub missing: BTreeSet<usize>,
    pub untitled: usize,
}

impl Violations {
    fn is_empty(&self) -> bool {
        self.unknown.is_empty()
            && self.duplicate.is_empty()
            && self.missing.is_empty()
            && self.untitled == 0
    }
}

impl fmt::Display for Violations {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let list = |ids: &BTreeSet<usize>| {
            let mut s: Vec<String> = ids.iter().take(40).map(usize::to_string).collect();
            if ids.len() > 40 {
                s.push(format!("… ({} in total)", ids.len()));
            }
            s.join(", ")
        };
        let mut parts = Vec::new();
        if !self.unknown.is_empty() {
            parts.push(format!("unknown hunk ids: {}", list(&self.unknown)));
        }
        if !self.duplicate.is_empty() {
            parts.push(format!(
                "hunk ids placed more than once: {}",
                list(&self.duplicate)
            ));
        }
        if !self.missing.is_empty() {
            parts.push(format!("missing hunk ids: {}", list(&self.missing)));
        }
        if self.untitled > 0 {
            parts.push(format!("{} group(s) without a title", self.untitled));
        }
        write!(f, "{}", parts.join("; "))
    }
}

/// Checks that every expected hunk is in exactly one layer of one group.
pub(super) fn check(drafts: &[Draft], expected: &BTreeSet<usize>) -> Result<(), Violations> {
    let mut v = Violations::default();
    let mut seen = BTreeSet::new();
    for d in drafts {
        if d.title.is_empty() {
            v.untitled += 1;
        }
        for id in d.hunk_ids() {
            if !expected.contains(&id) {
                v.unknown.insert(id);
            } else if !seen.insert(id) {
                v.duplicate.insert(id);
            }
        }
    }
    v.missing = expected.difference(&seen).copied().collect();
    if v.is_empty() { Ok(()) } else { Err(v) }
}

/// Keeps the valid part of an invalid grouping: unknown ids and repeats are
/// dropped (first placement wins), empty layers and groups removed.
/// Returns the kept groups and the hunks left without a group.
pub(super) fn salvage(drafts: Vec<Draft>, expected: &BTreeSet<usize>) -> (Vec<Draft>, Vec<usize>) {
    let mut seen = BTreeSet::new();
    let mut kept = Vec::new();
    for mut d in drafts {
        for l in &mut d.layers {
            l.hunk_ids
                .retain(|id| expected.contains(id) && seen.insert(*id));
        }
        d.layers.retain(|l| !l.hunk_ids.is_empty());
        if !d.layers.is_empty() {
            if d.title.is_empty() {
                d.title = "Untitled group".into();
            }
            kept.push(d);
        }
    }
    let left = expected.difference(&seen).copied().collect();
    (kept, left)
}

/// Merges layers with the same name inside each group, keeping first-seen
/// order, and drops empty layers and groups.
pub(super) fn normalize(drafts: &mut Vec<Draft>) {
    for d in drafts.iter_mut() {
        let mut layers: Vec<Layer> = Vec::new();
        for l in d.layers.drain(..) {
            match layers.iter_mut().find(|x| x.name == l.name) {
                Some(x) => x.hunk_ids.extend(l.hunk_ids),
                None => layers.push(l),
            }
        }
        layers.retain(|l| !l.hunk_ids.is_empty());
        d.layers = layers;
    }
    drafts.retain(|d| !d.layers.is_empty());
}

/// A merge of chunk groups proposed by the LLM.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Merge {
    pub groups: Vec<usize>,
    pub title: String,
    pub summary: String,
}

#[derive(Deserialize)]
struct RawMerges {
    merges: Vec<RawMerge>,
}

#[derive(Deserialize)]
struct RawMerge {
    groups: Vec<Id>,
    #[serde(default)]
    title: String,
    #[serde(default)]
    summary: String,
}

/// Parses and validates a merge answer against `count` groups.
pub(super) fn merges(text: &str, count: usize) -> Result<Vec<Merge>, String> {
    let raw: RawMerges = from_answer(text)?;
    let mut used = BTreeMap::new();
    let mut out = Vec::new();
    for (i, m) in raw.merges.into_iter().enumerate() {
        let mut groups: Vec<usize> = m.groups.into_iter().map(|g| g.0).collect();
        groups.sort_unstable();
        groups.dedup();
        if let Some(&g) = groups.iter().find(|&&g| g >= count) {
            return Err(format!("merge {i}: unknown group id {g}"));
        }
        for &g in &groups {
            if let Some(prev) = used.insert(g, i) {
                return Err(format!(
                    "group {g} appears in merges {prev} and {i}; each group may be merged once"
                ));
            }
        }
        if groups.len() < 2 {
            continue;
        }
        if m.title.trim().is_empty() {
            return Err(format!("merge {i} has no title"));
        }
        out.push(Merge {
            groups,
            title: m.title.trim().to_string(),
            summary: m.summary.trim().to_string(),
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_fenced_json() {
        assert_eq!(extract_json("```json\n{\"a\":1}\n```"), "{\"a\":1}");
        assert_eq!(
            extract_json("Here:\n```\n{\"a\":1}\n```\nDone"),
            "{\"a\":1}"
        );
        assert_eq!(extract_json("Sure! {\"a\":{}} hope it helps"), "{\"a\":{}}");
        assert_eq!(extract_json("nothing"), "nothing");
    }

    #[test]
    fn parses_lenient_ids_and_checks_invariants() {
        let d = groups(
            r#"{"groups":[{"title":"A","summary":"s","layers":[{"name":" API ","hunks":[0,"h1"]}]},
                {"title":"","layers":[{"name":"x","hunks":[1,7]}]}]}"#,
        )
        .unwrap();
        assert_eq!(d[0].layers[0].name, "api");
        let expected = BTreeSet::from([0, 1, 2]);
        let v = check(&d, &expected).unwrap_err();
        assert_eq!(v.unknown, BTreeSet::from([7]));
        assert_eq!(v.duplicate, BTreeSet::from([1]));
        assert_eq!(v.missing, BTreeSet::from([2]));
        assert_eq!(v.untitled, 1);
        assert_eq!(
            v.to_string(),
            "unknown hunk ids: 7; hunk ids placed more than once: 1; missing hunk ids: 2; 1 group(s) without a title"
        );

        let (kept, left) = salvage(d, &expected);
        assert_eq!(kept.len(), 1);
        assert_eq!(left, vec![2]);
    }

    #[test]
    fn validates_merges() {
        let ok = merges(
            r#"{"merges":[{"groups":[0,2],"title":"T","summary":"S"},{"groups":[1]}]}"#,
            3,
        )
        .unwrap();
        assert_eq!(ok.len(), 1);
        assert_eq!(ok[0].groups, vec![0, 2]);
        assert!(merges(r#"{"merges":[{"groups":[0,5],"title":"T"}]}"#, 3).is_err());
        assert!(
            merges(
                r#"{"merges":[{"groups":[0,1],"title":"T"},{"groups":[1,2],"title":"U"}]}"#,
                3
            )
            .is_err()
        );
    }
}
