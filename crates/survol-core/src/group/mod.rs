//! Stack view engine: hunks grouped by functional capability, then by
//! technical layer.
//!
//! Mechanical changes are set aside without LLM. The other hunks are
//! compressed into a prompt (chunked by module when too big, then merged),
//! and the answer is validated in code: every hunk lands in exactly one group.
//! Invalid answers get one retry, then fall back to grouping by directory.

mod fallback;
mod order;
mod parse;
mod prompt;

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde::{Deserialize, Serialize};

pub use order::{layer_rank, order_groups, order_groups_with_graph};

use crate::llm::{LlmProvider, LlmRequest};
use crate::mechanical::{self, Kind};
use crate::model::Diff;
use crate::review_state::ReviewState;
use fallback::Locale;

/// Version of the prompts in `prompts/`. Bump it when they change: it is part
/// of the cache key.
pub const PROMPT_VERSION: u32 = 4;

/// Chunks grouped at the same time.
const PARALLEL_CHUNKS: usize = 4;
/// Smallest chunk budget, whatever the configuration says.
const MIN_BUDGET: usize = 1_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Layer {
    /// `api`, `service`, `persistence`, `tests`...
    pub name: String,
    pub hunk_ids: Vec<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Group {
    pub id: usize,
    pub title: String,
    /// Functional explanation, 2–3 sentences.
    pub summary: String,
    pub layers: Vec<Layer>,
    /// Reading position; `groups` are sorted by it.
    pub order: usize,
    /// All hunks of the group, in diff order.
    pub hunk_ids: Vec<usize>,
    /// Files without hunks (pure renames, binaries), only in the mechanical group.
    #[serde(default)]
    pub file_ids: Vec<usize>,
    /// The "mechanical / noise" group, detected without LLM.
    pub mechanical: bool,
}

impl Group {
    /// Reviewed items (hunks and hunkless files), and total.
    pub fn progress(&self, diff: &Diff, state: &ReviewState) -> (usize, usize) {
        let hunks = self
            .hunk_ids
            .iter()
            .filter(|&&h| state.is_hunk_reviewed(diff, h))
            .count();
        let files = self
            .file_ids
            .iter()
            .filter(|&&f| state.is_file_reviewed(diff, f))
            .count();
        (hunks + files, self.hunk_ids.len() + self.file_ids.len())
    }

    pub fn is_reviewed(&self, diff: &Diff, state: &ReviewState) -> bool {
        let (done, total) = self.progress(diff, state);
        done == total
    }

    /// Validates (or un-validates) the whole group.
    pub fn set_reviewed(&self, diff: &Diff, state: &mut ReviewState, reviewed: bool) {
        for &h in &self.hunk_ids {
            state.set_hunk(diff, h, reviewed);
        }
        for &f in &self.file_ids {
            state.set_file(diff, f, reviewed);
        }
    }
}

/// How a grouping was produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// Valid LLM answer(s).
    Llm,
    /// Part of the hunks come from the LLM, the rest was grouped by directory.
    Partial,
    /// LLM unavailable or unusable: grouped by directory.
    Fallback,
    /// Only mechanical changes: no LLM needed.
    Mechanical,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Grouping {
    /// Sorted by [`Group::order`].
    pub groups: Vec<Group>,
    pub source: Source,
    pub model: Option<String>,
    pub prompt_version: u32,
    /// Cache key, see [`cache_key`].
    pub key: String,
    /// Loaded from the cache rather than computed.
    #[serde(default)]
    pub from_cache: bool,
    pub llm_calls: usize,
    /// What went wrong on the way (rejected answers, retries, fallbacks).
    pub warnings: Vec<String>,
}

impl Grouping {
    /// Reorders the groups along the code graph, see [`order_groups_with_graph`].
    /// [`build`] orders them by layer only; call this once the graph is ready.
    pub fn order_with_graph(&mut self, graph: &crate::graph::Graph) {
        order_groups_with_graph(&mut self.groups, graph);
    }

    /// Index in [`Self::groups`] of the group containing `hunk`.
    pub fn group_of_hunk(&self, hunk: usize) -> Option<usize> {
        self.groups.iter().position(|g| g.hunk_ids.contains(&hunk))
    }

    /// Reviewed items and total over all groups.
    pub fn progress(&self, diff: &Diff, state: &ReviewState) -> (usize, usize) {
        self.groups
            .iter()
            .map(|g| g.progress(diff, state))
            .fold((0, 0), |(a, b), (c, d)| (a + c, b + d))
    }
}

/// Inputs of a grouping, besides the diff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Params {
    pub model: Option<String>,
    pub effort: Option<String>,
    pub max_prompt_chars: usize,
    /// Project conventions (`.survol/instructions.md`).
    pub instructions: Option<String>,
    /// Language of titles and summaries, full English name (`French`), see
    /// [`crate::config::language_name`].
    pub language: String,
    /// Where the LLM runs from.
    pub cwd: PathBuf,
}

/// Groups of one chunk and how they were obtained.
type ChunkResult = (Vec<Draft>, Source);

/// A group before ids, order and hunk lists are assigned.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Draft {
    title: String,
    summary: String,
    layers: Vec<Layer>,
}

impl Draft {
    fn hunk_ids(&self) -> impl Iterator<Item = usize> + '_ {
        self.layers.iter().flat_map(|l| l.hunk_ids.iter().copied())
    }
}

/// Groups the hunks of `diff`. Never fails: LLM problems end up in
/// [`Grouping::warnings`] and a directory-based fallback.
pub fn build(
    diff: &Diff,
    params: &Params,
    llm: &dyn LlmProvider,
    progress: &mut dyn FnMut(&str),
) -> Grouping {
    build_with(diff, params, Some(llm), progress)
}

/// Groups the hunks of `diff` without LLM: mechanical group plus grouping by
/// directory (source [`Source::Fallback`], no warning).
pub fn build_offline(diff: &Diff, params: &Params) -> Grouping {
    build_with(diff, params, None, &mut |_| {})
}

fn build_with(
    diff: &Diff,
    params: &Params,
    llm: Option<&dyn LlmProvider>,
    progress: &mut dyn FnMut(&str),
) -> Grouping {
    let mut mech: BTreeMap<Kind, Vec<usize>> = BTreeMap::new();
    let mut rest = Vec::new();
    for h in &diff.hunks {
        match mechanical::hunk_kind(diff, h) {
            Some(k) => mech.entry(k).or_default().push(h.id),
            None => rest.push(h.id),
        }
    }
    let files: Vec<usize> = mechanical::hunkless_files(diff).collect();

    let mut run = Run {
        diff,
        params,
        llm,
        calls: AtomicUsize::new(0),
        warnings: Mutex::new(Vec::new()),
    };
    let (drafts, source) = if rest.is_empty() {
        (Vec::new(), Source::Mechanical)
    } else if run.llm.is_none() {
        (
            fallback::by_directory(diff, &rest, &params.language),
            Source::Fallback,
        )
    } else {
        run.group_all(&rest, progress)
    };

    let mut groups: Vec<Group> = drafts
        .into_iter()
        .enumerate()
        .map(|(id, d)| {
            let mut hunk_ids: Vec<usize> = d.hunk_ids().collect();
            hunk_ids.sort_unstable();
            Group {
                id,
                title: d.title,
                summary: d.summary,
                layers: d.layers,
                order: 0,
                hunk_ids,
                file_ids: Vec::new(),
                mechanical: false,
            }
        })
        .collect();
    if !mech.is_empty() || !files.is_empty() {
        groups.push(mechanical_group(
            groups.len(),
            mech,
            files,
            Locale::of(&params.language),
        ));
    }
    order_groups(&mut groups);

    Grouping {
        groups,
        source,
        model: params.model.clone(),
        prompt_version: PROMPT_VERSION,
        key: cache_key(diff, params),
        from_cache: false,
        llm_calls: *run.calls.get_mut(),
        warnings: std::mem::take(run.warnings.get_mut().expect("no poisoning")),
    }
}

fn mechanical_group(
    id: usize,
    mech: BTreeMap<Kind, Vec<usize>>,
    files: Vec<usize>,
    locale: Locale,
) -> Group {
    let fr = locale == Locale::Fr;
    let mut parts = Vec::new();
    let count = |k| mech.get(&k).map_or(0, Vec::len);
    let generated = count(Kind::Generated);
    if generated > 0 {
        parts.push(if fr {
            format!("{generated} hunk(s) dans des lockfiles ou des fichiers générés")
        } else {
            format!("{generated} hunk(s) in lockfiles or generated files")
        });
    }
    let whitespace = count(Kind::Whitespace);
    if whitespace > 0 {
        parts.push(if fr {
            format!("{whitespace} hunk(s) ne touchant que des blancs")
        } else {
            format!("{whitespace} whitespace-only hunk(s)")
        });
    }
    if !files.is_empty() {
        let n = files.len();
        parts.push(if fr {
            format!(
                "{n} fichier(s) sans modification de contenu (renommages, copies, binaires, droits)"
            )
        } else {
            format!("{n} file(s) without content changes (renames, copies, binaries, mode changes)")
        });
    }
    let layers: Vec<Layer> = mech
        .into_iter()
        .map(|(k, hunk_ids)| Layer {
            name: k.layer().to_string(),
            hunk_ids,
        })
        .collect();
    let mut hunk_ids: Vec<usize> = layers.iter().flat_map(|l| l.hunk_ids.clone()).collect();
    hunk_ids.sort_unstable();
    Group {
        id,
        title: if fr {
            "Modifications mécaniques"
        } else {
            "Mechanical changes"
        }
        .into(),
        summary: format!(
            "{}. {}",
            capitalize(&parts.join(", ")),
            if fr {
                "Détecté sans LLM : rien à comprendre ici, vérifier et valider d'un coup."
            } else {
                "Detected without LLM: nothing to understand here, check and validate at once."
            }
        ),
        layers,
        order: 0,
        hunk_ids,
        file_ids: files,
        mechanical: true,
    }
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    c.next()
        .map(|f| f.to_uppercase().chain(c).collect())
        .unwrap_or_default()
}

/// Shared state of one grouping run (chunks may run in parallel).
struct Run<'a> {
    diff: &'a Diff,
    params: &'a Params,
    /// `None`: LLM disabled, only [`build_offline`] runs.
    llm: Option<&'a dyn LlmProvider>,
    calls: AtomicUsize,
    warnings: Mutex<Vec<String>>,
}

impl Run<'_> {
    fn warn(&self, msg: String) {
        self.warnings.lock().expect("no poisoning").push(msg);
    }

    fn complete(&self, prompt: String) -> Result<String, crate::llm::LlmError> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        let llm = self.llm.expect("the LLM is only called when enabled");
        llm.complete(&LlmRequest {
            prompt,
            model: self.params.model.clone(),
            effort: self.params.effort.clone(),
            cwd: self.params.cwd.clone(),
        })
    }

    fn group_all(&self, hunks: &[usize], progress: &mut dyn FnMut(&str)) -> (Vec<Draft>, Source) {
        let instr = self.params.instructions.as_deref();
        let budget = self
            .params
            .max_prompt_chars
            .saturating_sub(prompt::group_overhead(instr, &self.params.language))
            .max(MIN_BUDGET);
        let blocks = prompt::blocks(self.diff, hunks, budget);
        let size: usize = blocks.iter().map(|b| b.text.len()).sum();
        let chunks = if size <= budget {
            vec![blocks]
        } else {
            prompt::chunks(blocks, budget)
        };

        let n = chunks.len();
        if n == 1 {
            progress(&format!("grouping {} hunks", hunks.len()));
            return self.group_chunk(&chunks[0], None);
        }
        progress(&format!(
            "grouping {} hunks in {n} chunks by module",
            hunks.len()
        ));
        let results = self.group_chunks(&chunks, progress);
        let first = results[0].1;
        let source = if results.iter().all(|r| r.1 == first) {
            first
        } else {
            Source::Partial
        };
        let drafts: Vec<Draft> = results.into_iter().flat_map(|r| r.0).collect();
        progress("merging groups across modules");
        (self.merge(drafts), source)
    }

    /// Groups chunks in parallel, reporting each completion.
    fn group_chunks(
        &self,
        chunks: &[Vec<prompt::Block>],
        progress: &mut dyn FnMut(&str),
    ) -> Vec<ChunkResult> {
        let n = chunks.len();
        let next = AtomicUsize::new(0);
        let results: Vec<Mutex<Option<ChunkResult>>> = (0..n).map(|_| Mutex::new(None)).collect();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::scope(|s| {
            for _ in 0..PARALLEL_CHUNKS.min(n) {
                let tx = tx.clone();
                let (next, results) = (&next, &results);
                s.spawn(move || {
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        if i >= n {
                            break;
                        }
                        let r = self.group_chunk(&chunks[i], Some(i));
                        *results[i].lock().expect("no poisoning") = Some(r);
                        let _ = tx.send(i);
                    }
                });
            }
            drop(tx);
            for (done, _) in rx.iter().enumerate() {
                progress(&format!("grouped chunk {}/{n}", done + 1));
            }
        });
        results
            .into_iter()
            .map(|m| {
                m.into_inner()
                    .expect("no poisoning")
                    .expect("every chunk done")
            })
            .collect()
    }

    /// Asks the LLM, retries once with the error, then falls back.
    fn group_chunk(&self, blocks: &[prompt::Block], chunk: Option<usize>) -> ChunkResult {
        let tag = chunk.map_or(String::new(), |i| format!("chunk {}: ", i + 1));
        let expected: BTreeSet<usize> = blocks.iter().flat_map(|b| b.hunk_ids.clone()).collect();
        let base = prompt::group_prompt(
            blocks,
            self.params.instructions.as_deref(),
            &self.params.language,
        );
        let mut prompt = base.clone();
        let mut best = None;
        for attempt in 1..=2 {
            let answer = match self.complete(prompt) {
                Ok(a) => a,
                Err(e) => {
                    self.warn(format!("{tag}LLM call failed: {e}"));
                    break;
                }
            };
            let error = match parse::groups(&answer) {
                Ok(mut drafts) => {
                    parse::normalize(&mut drafts);
                    match parse::check(&drafts, &expected) {
                        Ok(()) => return (drafts, Source::Llm),
                        Err(v) => {
                            best = Some(drafts);
                            v.to_string()
                        }
                    }
                }
                Err(e) => e,
            };
            self.warn(format!("{tag}answer {attempt} rejected: {error}"));
            prompt = retry_prompt(&base, &answer, &error);
        }

        if let Some(drafts) = best {
            let (mut kept, left) = parse::salvage(drafts, &expected);
            if !kept.is_empty() {
                if !left.is_empty() {
                    self.warn(format!(
                        "{tag}{} hunk(s) left out by the LLM, grouped by directory",
                        left.len()
                    ));
                    kept.extend(fallback::by_directory(
                        self.diff,
                        &left,
                        &self.params.language,
                    ));
                }
                return (kept, Source::Partial);
            }
        }
        self.warn(format!("{tag}falling back to grouping by directory"));
        let all: Vec<usize> = expected.into_iter().collect();
        (
            fallback::by_directory(self.diff, &all, &self.params.language),
            Source::Fallback,
        )
    }

    /// Lets the LLM merge groups of different chunks that share a capability.
    /// On failure the groups are kept unmerged.
    fn merge(&self, drafts: Vec<Draft>) -> Vec<Draft> {
        if drafts.len() < 2 {
            return drafts;
        }
        let base = prompt::merge_prompt(
            &drafts,
            self.diff,
            self.params.instructions.as_deref(),
            &self.params.language,
        );
        let mut prompt = base.clone();
        for attempt in 1..=2 {
            let answer = match self.complete(prompt) {
                Ok(a) => a,
                Err(e) => {
                    self.warn(format!("merge: LLM call failed: {e}"));
                    break;
                }
            };
            match parse::merges(&answer, drafts.len()) {
                Ok(merges) => return apply_merges(drafts, merges),
                Err(e) => {
                    self.warn(format!("merge: answer {attempt} rejected: {e}"));
                    prompt = format!(
                        "{base}\n\n# Correction\n\nYour previous answer was rejected: {e}.\n\
                         Answer again with the complete JSON object."
                    );
                }
            }
        }
        self.warn("merge: groups of different modules left unmerged".into());
        drafts
    }
}

/// The original prompt, the rejected answer and why it was rejected: the
/// model fixes its answer rather than starting over.
fn retry_prompt(base: &str, answer: &str, error: &str) -> String {
    format!(
        "{base}\n\n# Correction\n\nYour previous answer was:\n\n{}\n\n\
         It was rejected: {error}.\n\
         Answer again with the complete corrected JSON object, placing every hunk id exactly once.",
        parse::extract_json(answer)
    )
}

fn apply_merges(drafts: Vec<Draft>, merges: Vec<parse::Merge>) -> Vec<Draft> {
    let mut slots: Vec<Option<Draft>> = drafts.into_iter().map(Some).collect();
    let by_first: BTreeMap<usize, parse::Merge> =
        merges.into_iter().map(|m| (m.groups[0], m)).collect();
    let mut out = Vec::new();
    for i in 0..slots.len() {
        match by_first.get(&i) {
            Some(m) => {
                let members: Vec<Draft> =
                    m.groups.iter().filter_map(|&g| slots[g].take()).collect();
                let summary = if m.summary.is_empty() {
                    members[0].summary.clone()
                } else {
                    m.summary.clone()
                };
                let mut merged = vec![Draft {
                    title: m.title.clone(),
                    summary,
                    layers: members.into_iter().flat_map(|d| d.layers).collect(),
                }];
                parse::normalize(&mut merged);
                out.extend(merged);
            }
            None => out.extend(slots[i].take()),
        }
    }
    out
}

/// Identity of a grouping: hunk contents, mechanical classification, prompt
/// version, model, effort, budget, language and project instructions.
pub fn cache_key(diff: &Diff, params: &Params) -> String {
    let mut h = blake3::Hasher::new();
    h.update(format!("survol-groups\0{PROMPT_VERSION}\0").as_bytes());
    h.update(
        format!(
            "{:?}\0{:?}\0{}\0{}\0",
            params.model, params.effort, params.max_prompt_chars, params.language
        )
        .as_bytes(),
    );
    h.update(params.instructions.as_deref().unwrap_or("").as_bytes());
    for hunk in &diff.hunks {
        let kind = mechanical::hunk_kind(diff, hunk);
        h.update(format!("\0{}{kind:?}", hunk.content_hash).as_bytes());
    }
    for f in mechanical::hunkless_files(diff) {
        h.update(format!("\0{}", diff.file_key(f)).as_bytes());
    }
    h.finalize().to_hex()[..32].to_string()
}

/// `.git/survol/cache/<head_sha>/groups.json`
pub fn cache_path(survol_dir: &Path, head_sha: &str) -> PathBuf {
    survol_dir.join("cache").join(head_sha).join("groups.json")
}

/// The cached grouping, if there is one for `key`.
pub fn load_cache(path: &Path, key: &str) -> Option<Grouping> {
    let bytes = std::fs::read(path).ok()?;
    let mut g: Grouping = serde_json::from_slice(&bytes).ok()?;
    (g.key == key).then(|| {
        g.from_cache = true;
        g
    })
}

/// Writes atomically.
pub fn save_cache(path: &Path, grouping: &Grouping) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(
        &tmp,
        serde_json::to_vec_pretty(grouping).map_err(io::Error::other)?,
    )?;
    std::fs::rename(tmp, path)
}

#[cfg(test)]
mod tests;
