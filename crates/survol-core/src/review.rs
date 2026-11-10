//! Opening a review: resolve the target, fetch, diff, and locate the worktree.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::forge::{
    self, Capabilities, Discussion, Forge, ForgeError, ForgeKind, ForgeTarget, MrRef,
    project_from_remote,
};
use crate::git::Git;
use crate::graph::{self, Graph};
use crate::group::{self, Grouping};
use crate::index::{self, Index};
use crate::llm::LlmProvider;
use crate::model::{Diff, FileStatus, MergeRequest, Side};
use crate::renames::{self, Renames};
use crate::{Error, Result, ask, comments, diff, lsp, mechanical};

/// What to review.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// A merge request; `None` means the one of the current branch.
    MergeRequest(Option<MrRef>),
    /// A local range `base..head`, diffed from their merge base like GitLab.
    /// `head` may be [`WORKTREE`] or [`INDEX`]: the uncommitted changes.
    Range { base: String, head: String },
}

/// Head of a range standing for the working tree: the tracked files as they
/// are on disk and the untracked ones git does not ignore.
pub const WORKTREE: &str = "WORKTREE";
/// Head of a range standing for the index (the staged changes).
pub const INDEX: &str = "INDEX";

impl Target {
    /// `123`, `!123`, `#123`, a MR / PR URL, `base..head`, `WORKTREE` /
    /// `INDEX` (alone: from `HEAD`), or nothing.
    pub fn parse(arg: Option<&str>) -> Result<Self> {
        let Some(arg) = arg.map(str::trim).filter(|a| !a.is_empty()) else {
            return Ok(Self::MergeRequest(None));
        };
        if arg == WORKTREE || arg == INDEX {
            return Ok(Self::Range {
                base: "HEAD".into(),
                head: arg.into(),
            });
        }
        if let Some(mr) = MrRef::parse(arg) {
            return Ok(Self::MergeRequest(Some(mr)));
        }
        if let Some((base, head)) = arg.split_once("..") {
            let head = head.trim_start_matches('.');
            return Ok(Self::Range {
                base: base.to_string(),
                head: if head.is_empty() { "HEAD" } else { head }.to_string(),
            });
        }
        Err(Error::Target(format!(
            "`{arg}` is neither a merge / pull request (number or URL), a range base..head, \
             WORKTREE nor INDEX"
        )))
    }
}

/// A review ready to display.
#[derive(Debug, Clone)]
pub struct Review {
    pub mr: Option<MergeRequest>,
    pub base_sha: String,
    pub head_sha: String,
    pub diff: Diff,
    /// What the head stands for when it is not a commit of the repository:
    /// [`WORKTREE`] or [`INDEX`] (the head is then a snapshot commit).
    pub head_label: Option<String>,
    /// Where the head is checked out. Created by [`Review::ensure_worktree`].
    pub worktree: PathBuf,
    /// Key of the review state, see [`crate::review_state::state_path`].
    pub state_key: String,
    pub repo: Git,
}

impl Review {
    pub fn title(&self) -> String {
        match &self.mr {
            Some(mr) => format!("{} {}", mr.reference(), mr.title),
            None => match self.head_label.as_deref() {
                Some(WORKTREE) => format!("{}..working tree", short(&self.base_sha)),
                Some(INDEX) => format!("{}..index", short(&self.base_sha)),
                _ => format!("{}..{}", short(&self.base_sha), short(&self.head_sha)),
            },
        }
    }

    /// The forge of the merge request (GitLab for a local range).
    pub fn forge(&self) -> ForgeKind {
        self.mr.as_ref().map(|m| m.forge).unwrap_or_default()
    }

    pub fn state_path(&self) -> Result<PathBuf> {
        Ok(crate::review_state::state_path(
            &self.repo.survol_dir()?,
            &self.state_key,
        ))
    }

    /// Checks out the head in the dedicated worktree (no-op if already there).
    pub fn ensure_worktree(&self) -> Result<()> {
        if self.worktree == self.repo.dir() {
            return Ok(());
        }
        Ok(self.repo.ensure_worktree(&self.worktree, &self.head_sha)?)
    }
}

pub fn short(sha: &str) -> &str {
    &sha[..sha.len().min(8)]
}

/// Resolves `target`, fetches what is missing and computes the diff.
/// `progress` receives short status messages.
pub fn open(
    repo: &Git,
    cfg: &Config,
    target: &Target,
    mut progress: impl FnMut(&str),
) -> Result<Review> {
    let mut head_label = None;
    let (mr, base_sha, head_sha, state_key, worktree) = match target {
        Target::MergeRequest(mr_ref) => {
            progress("fetching merge request metadata");
            let remote_url = repo.remote_url(&cfg.git.remote).ok();
            let target = forge::detect(cfg, remote_url.as_deref(), mr_ref.as_ref());
            let forge = forge::connect(cfg, &target)?;
            let mut mr = resolve_mr(repo, cfg, forge.as_ref(), mr_ref.as_ref())?;
            fetch_shas(repo, cfg, &mr, &mut progress)?;
            if mr.forge == ForgeKind::Github {
                // GitHub diffs from the merge base, which the API does not give.
                mr.base_sha = repo.merge_base(&mr.start_sha, &mr.head_sha)?;
            }
            let key = format!("mr-{}", mr.iid);
            let wt = repo.survol_dir()?.join("worktrees").join(&key);
            (Some(mr.clone()), mr.base_sha, mr.head_sha, key, wt)
        }
        Target::Range { base, head } => {
            let head_sha = match head.as_str() {
                WORKTREE => {
                    progress("snapshotting the working tree");
                    head_label = Some(WORKTREE.to_string());
                    repo.snapshot_worktree()?
                }
                INDEX => {
                    head_label = Some(INDEX.to_string());
                    repo.snapshot_index()?
                }
                _ => repo.rev_parse(head)?,
            };
            let base_sha = repo.text(&["merge-base", &repo.rev_parse(base)?, &head_sha])?;
            let key = format!("range-{}", sanitize(&format!("{base}..{head}")));
            // Reviewing the checked-out commit or the working tree: the repo
            // itself is the worktree.
            let wt = if head == WORKTREE
                || repo.rev_parse("HEAD").ok().as_deref() == Some(head_sha.as_str())
            {
                repo.dir().to_path_buf()
            } else {
                repo.survol_dir()?.join("worktrees").join(&key)
            };
            (None, base_sha, head_sha, key, wt)
        }
    };

    progress("computing diff");
    let raw = repo.diff(&base_sha, &head_sha)?;
    let mut diff = diff::parse(&raw)?;
    diff.sort_as_tree();
    mechanical::mark(
        &mut diff,
        &mechanical::globset(&cfg.review.mechanical_globs)?,
    );

    Ok(Review {
        mr,
        base_sha,
        head_sha,
        diff,
        head_label,
        worktree,
        state_key,
        repo: repo.clone(),
    })
}

/// Groups the review's hunks for the Stack view. With `use_cache`, a grouping
/// cached for the same hunks, prompts, model and instructions is returned
/// without calling the LLM. Directory fallbacks are not cached, so that the
/// next run tries the LLM again. With `[llm] enabled = false`, `llm` is never
/// called: hunks are grouped by directory and nothing is cached.
pub fn group(
    review: &Review,
    cfg: &Config,
    llm: &dyn LlmProvider,
    mut progress: impl FnMut(&str),
    use_cache: bool,
) -> Result<Grouping> {
    let cwd = llm_cwd(review);
    let params = group::Params {
        model: cfg.llm.group_model.clone(),
        effort: cfg.llm.group_effort.clone(),
        max_prompt_chars: cfg.llm.max_prompt_chars,
        instructions: read_instructions(review.repo.dir())?,
        language: cfg.llm.language(),
        cwd,
        renames: if cfg.review.fold_renames {
            progress("looking for renames");
            renames(review, cfg)?
        } else {
            Renames::default()
        },
    };
    if !cfg.llm.enabled {
        progress("LLM disabled: grouping by directory");
        return Ok(group::build_offline(&review.diff, &params));
    }
    let survol = review.repo.survol_dir()?;
    let path = if cfg.review.fold_renames {
        group::cache_path(&survol, &review.head_sha)
    } else {
        group::unfolded_cache_path(&survol, &review.head_sha)
    };
    if use_cache && let Some(g) = group::load_cache(&path, &group::cache_key(&review.diff, &params))
    {
        progress("groups loaded from cache");
        return Ok(g);
    }
    let g = group::build(&review.diff, &params, llm, &mut progress);
    if g.source != group::Source::Fallback {
        group::save_cache(&path, &g)?;
    }
    Ok(g)
}

/// Version of [`renames::detect`]: part of the cache key of [`renames`].
const RENAMES_VERSION: u32 = 1;

#[derive(Serialize, Deserialize)]
struct CachedRenames {
    key: String,
    renames: Renames,
}

/// The hunks of the review that only follow a rename or a move. Looks the
/// candidate names up in the trees of the base and the head (`git grep`),
/// so it is cached under `.git/survol/cache/<head_sha>/renames.json`.
pub fn renames(review: &Review, cfg: &Config) -> Result<Renames> {
    let path = review
        .repo
        .survol_dir()?
        .join("cache")
        .join(&review.head_sha)
        .join("renames.json");
    let key = format!(
        "{RENAMES_VERSION}\0{}\0{:?}",
        review.base_sha, cfg.review.mechanical_globs
    );
    if let Some(c) = std::fs::read(&path)
        .ok()
        .and_then(|b| serde_json::from_slice::<CachedRenames>(&b).ok())
        .filter(|c| c.key == key)
    {
        return Ok(c.renames);
    }
    let renames = renames::detect(&review.diff, |side, names| {
        let rev = match side {
            Side::Old => &review.base_sha,
            Side::New => &review.head_sha,
        };
        let mut found = HashSet::new();
        let patterns: Vec<&str> = names.iter().map(String::as_str).collect();
        review.repo.grep_lines(rev, &patterns, |line| {
            for word in renames::words(line) {
                if let Ok(w) = std::str::from_utf8(word)
                    && names.contains(w)
                    && !found.contains(w)
                {
                    found.insert(w.to_string());
                }
            }
            found.len() < names.len()
        })?;
        Ok::<_, Error>(found)
    })?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let cached = CachedRenames { key, renames };
    std::fs::write(
        &path,
        serde_json::to_vec(&cached).map_err(std::io::Error::other)?,
    )?;
    Ok(cached.renames)
}

/// Builds the code graph of the review: tree-sitter index of the head (and
/// of the base version of the changed files), hunk → symbol mapping,
/// resolution and [`graph::default_rules`]. Reads everything from git
/// objects, so it does not need the worktree; safe to run on a background
/// thread. Parsed files are cached by blob under `.git/survol/cache/index/`
/// and the graph under `.git/survol/cache/<head_sha>/graph.json`; with
/// `use_cache`, a graph cached for the same revisions, versions and rules is
/// returned as is.
pub fn build_graph(
    review: &Review,
    cfg: &Config,
    mut progress: impl FnMut(&str),
    use_cache: bool,
) -> Result<Graph> {
    let survol = review.repo.survol_dir()?;
    let rules = graph::default_rules();
    let globs = &cfg.review.mechanical_globs;
    let key = graph::cache_key(&review.base_sha, &review.head_sha, &rules, globs);
    let path = graph::cache_path(&survol, &review.head_sha);
    if use_cache && let Some(g) = graph::load_cache(&path, &key) {
        progress("graph loaded from cache");
        return Ok(g);
    }
    let opts = index::Options::new(globs, Some(&survol))?;
    let base_paths: Vec<String> = review
        .diff
        .files
        .iter()
        .filter(|f| f.status != FileStatus::Added)
        .map(|f| f.old_path.clone().unwrap_or_else(|| f.path.clone()))
        .collect();
    let index = Index::build(
        &review.repo,
        &review.head_sha,
        Some((&review.base_sha, &base_paths)),
        &opts,
        &mut progress,
    )?;
    let s = &index.stats;
    progress(&format!(
        "indexed {} files in {} ms ({} parsed, {} from cache, {} skipped)",
        s.files, s.millis, s.parsed, s.cached, s.skipped
    ));
    let mut g = graph::build(&index, &review.diff, &rules);
    g.set_origin(key, review.base_sha.clone(), review.head_sha.clone());
    let s = g.stats();
    progress(&format!(
        "graph: {} symbols, {} edges, {}/{} references resolved in {} ms",
        s.symbols, s.edges, s.resolved, s.refs, s.millis
    ));
    graph::save_cache(&path, &g)?;
    Ok(g)
}

/// A refined graph in the cache, with its key (see [`lsp::cache_key`]).
#[derive(serde::Serialize, serde::Deserialize)]
struct CachedRefinement {
    key: String,
    graph: Graph,
}

/// Refines `graph` with language servers started on the review worktree
/// (checked out if needed), see [`lsp`]. Bounded by `[lsp] budget_secs`;
/// never fails because of a server: missing or failing ones are reported
/// in [`Graph::lsp_stats`]. `None` when `[lsp] enabled = false` or nothing
/// touches a changed callable. With `use_cache`, a refinement cached for
/// the same graph and servers is returned at once. Blocking: run it on a
/// background thread.
pub fn refine_graph(
    review: &Review,
    cfg: &Config,
    graph: &Graph,
    progress: &(dyn Fn(&str) + Sync),
    use_cache: bool,
) -> Result<Option<Graph>> {
    if !cfg.lsp.enabled {
        return Ok(None);
    }
    let survol = review.repo.survol_dir()?;
    let key = lsp::cache_key(&graph.data().key, &cfg.lsp);
    let path = lsp::cache_path(&survol, &review.head_sha);
    if use_cache
        && let Ok(bytes) = std::fs::read(&path)
        && let Ok(c) = serde_json::from_slice::<CachedRefinement>(&bytes)
        && c.key == key
    {
        let mut g = c.graph;
        g.from_cache = true;
        if let Some(s) = g.lsp_stats() {
            progress(&format!("{} (cached)", s.summary()));
        }
        return Ok(Some(g));
    }
    if !review.worktree.is_dir() {
        progress("LSP: checking out the worktree");
        review.ensure_worktree()?;
    }
    let data_dir = survol.join("lsp");
    let opts = lsp::Options {
        root: &review.worktree,
        data_dir: Some(&data_dir),
        cfg: &cfg.lsp,
    };
    let (refined, stats) = lsp::refine(graph, &opts, progress);
    if stats.tasks == 0 {
        return Ok(None);
    }
    progress(&stats.summary());
    if stats.any_ready() {
        let cached = CachedRefinement {
            key,
            graph: refined,
        };
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(
            &tmp,
            serde_json::to_vec(&cached).map_err(std::io::Error::from)?,
        )?;
        std::fs::rename(tmp, &path)?;
        return Ok(Some(cached.graph));
    }
    Ok(Some(refined))
}

/// `.survol/instructions.md`: the team's architecture conventions, if any.
pub fn instructions_path(repo_root: &Path) -> PathBuf {
    repo_root.join(".survol/instructions.md")
}

/// The project's `.survol/instructions.md`, if any.
pub fn instructions(review: &Review) -> Result<Option<String>> {
    read_instructions(review.repo.dir())
}

/// `path` at `rev`, read from git objects; `None` if it does not exist there.
pub fn read_file(repo: &Git, rev: &str, path: &str) -> Option<String> {
    let bytes = repo.bytes(&["show", &format!("{rev}:{path}")]).ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// Where the LLM runs: the worktree once checked out, else the repository.
pub fn llm_cwd(review: &Review) -> PathBuf {
    if review.worktree.is_dir() {
        review.worktree.clone()
    } else {
        review.repo.dir().to_path_buf()
    }
}

/// Builds the prompt of a question about `subject`, from the review, its
/// graph and grouping when available. Reads code from git objects.
pub fn ask_prompt(
    review: &Review,
    cfg: &Config,
    graph: Option<&Graph>,
    grouping: Option<&Grouping>,
    subject: &ask::Subject,
    question: &str,
) -> Result<ask::Prompt> {
    let read = |rev: &str, path: &str| read_file(&review.repo, rev, path);
    let src = ask::Sources {
        diff: &review.diff,
        graph,
        grouping,
        read: &read,
        base_sha: &review.base_sha,
        head_sha: &review.head_sha,
    };
    let instructions = instructions(review)?;
    Ok(ask::build_prompt(
        &src,
        subject,
        question,
        instructions.as_deref(),
        &cfg.llm.language(),
    )?)
}

/// How to ask questions about this review: `[llm] ask_model`, answers cached
/// under `.git/survol/cache/<head>/ask/`.
pub fn ask_params(review: &Review, cfg: &Config, use_cache: bool) -> Result<ask::Params> {
    Ok(ask::Params {
        model: cfg.llm.ask_model.clone(),
        cwd: llm_cwd(review),
        cache_dir: Some(ask::cache_dir(&review.repo.survol_dir()?, &review.head_sha)),
        use_cache,
        head_sha: review.head_sha.clone(),
    })
}

/// `.git/survol/reviews/<key>/questions.json`
pub fn questions_path(review: &Review) -> Result<PathBuf> {
    Ok(ask::history_path(
        &review.repo.survol_dir()?,
        &review.state_key,
    ))
}

/// `.git/survol/reviews/<key>/comments.json`
pub fn comments_path(review: &Review) -> Result<PathBuf> {
    Ok(comments::comments_path(
        &review.repo.survol_dir()?,
        &review.state_key,
    ))
}

/// The SHAs positions refer to: the merge request's, or for a local range
/// its merge base (as base and start) and head.
pub fn shas(review: &Review) -> comments::Shas {
    match &review.mr {
        Some(mr) => comments::Shas {
            base: mr.base_sha.clone(),
            start: mr.start_sha.clone(),
            head: mr.head_sha.clone(),
        },
        None => comments::Shas {
            base: review.base_sha.clone(),
            start: review.base_sha.clone(),
            head: review.head_sha.clone(),
        },
    }
}

/// What the forge knows about a merge request's review.
#[derive(Debug, Clone, serde::Serialize)]
pub struct RemoteReview {
    pub capabilities: Capabilities,
    pub discussions: Vec<Discussion>,
    /// The current user's draft notes not published yet (made in the
    /// browser, or left by an interrupted publication).
    pub pending_drafts: usize,
}

/// A client for the forge of `mr`.
pub fn connect(mr: &MergeRequest, cfg: &Config) -> Result<Box<dyn Forge + Send>> {
    let host = Some(mr.host.clone()).filter(|h| !h.is_empty());
    let target = ForgeTarget {
        kind: mr.forge,
        host: match mr.forge {
            // GitLab: the configured host, as before (else the URL's).
            ForgeKind::Gitlab => cfg.gitlab.host.clone().or(host),
            ForgeKind::Github => host,
        },
        reason: String::new(),
    };
    Ok(forge::connect(cfg, &target)?)
}

/// Instance version, discussions and pending drafts of the review's merge
/// request; `None` for a local range. Network calls: run in the background.
pub fn fetch_remote(review: &Review, cfg: &Config) -> Result<Option<RemoteReview>> {
    let Some(mr) = &review.mr else {
        return Ok(None);
    };
    let forge = connect(mr, cfg)?;
    let capabilities = forge.capabilities()?;
    let discussions = forge.discussions(&mr.project, mr.iid)?;
    let pending_drafts = if capabilities.draft_notes {
        forge
            .draft_notes(&mr.project, mr.iid)
            .map_or(0, |d| d.len())
    } else {
        0
    };
    Ok(Some(RemoteReview {
        capabilities,
        discussions,
        pending_drafts,
    }))
}

/// Publishes the drafts of `store` (saved at `path`) to the merge request
/// with `plan`, see [`comments::publish`].
pub fn publish(
    review: &Review,
    cfg: &Config,
    plan: &comments::Plan,
    store: &mut comments::CommentStore,
    path: &Path,
    progress: &mut dyn FnMut(&str),
) -> Result<usize> {
    let Some(mr) = &review.mr else {
        return Err(Error::Target(
            "publishing needs a merge request: the drafts of a local range stay local".into(),
        ));
    };
    let forge = connect(mr, cfg)?;
    comments::publish(
        forge.as_ref(),
        &mr.project,
        mr.iid,
        plan,
        store,
        path,
        progress,
    )
    .map_err(|e| Error::Target(e.to_string()))
}

fn read_instructions(repo_root: &Path) -> Result<Option<String>> {
    match std::fs::read_to_string(instructions_path(repo_root)) {
        Ok(s) => Ok(Some(s).filter(|s| !s.trim().is_empty())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

fn resolve_mr(
    repo: &Git,
    cfg: &Config,
    forge: &dyn Forge,
    mr_ref: Option<&MrRef>,
) -> Result<MergeRequest> {
    let project = match mr_ref.and_then(|r| r.project.clone()) {
        Some(p) => p,
        None => {
            let url = repo.remote_url(&cfg.git.remote)?;
            project_from_remote(&url)
                .ok_or_else(|| ForgeError::Project(format!("unrecognised remote URL `{url}`")))?
        }
    };
    let iid = match mr_ref {
        Some(r) => r.iid,
        None => {
            let branch = repo.current_branch()?.ok_or_else(|| {
                Error::Target("detached HEAD: pass a merge request number".into())
            })?;
            forge.merge_request_for_branch(&project, &branch)?
        }
    };
    Ok(forge.merge_request(&project, iid)?)
}

fn fetch_shas(
    repo: &Git,
    cfg: &Config,
    mr: &MergeRequest,
    progress: &mut impl FnMut(&str),
) -> Result<()> {
    let remote = cfg.git.remote.as_str();
    if !repo.has_commit(&mr.head_sha) {
        progress("fetching merge request head");
        repo.fetch_merge_request(remote, mr.forge, mr.iid)?;
    }
    for sha in [&mr.base_sha, &mr.start_sha] {
        if !repo.has_commit(sha) {
            progress("fetching target branch");
            repo.fetch(remote, &[&mr.target_branch])?;
            if !repo.has_commit(sha) {
                repo.fetch(remote, &[sha])?;
            }
        }
    }
    if !repo.has_commit(&mr.head_sha) {
        return Err(Error::Target(format!(
            "head {} of {} is not reachable after fetch",
            short(&mr.head_sha),
            mr.reference()
        )));
    }
    Ok(())
}

fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::testutil::*;

    #[test]
    fn parses_targets() {
        assert_eq!(Target::parse(None).unwrap(), Target::MergeRequest(None));
        assert!(matches!(
            Target::parse(Some("12")).unwrap(),
            Target::MergeRequest(Some(_))
        ));
        assert_eq!(
            Target::parse(Some("main...feat")).unwrap(),
            Target::Range {
                base: "main".into(),
                head: "feat".into()
            }
        );
        assert_eq!(
            Target::parse(Some("main..")).unwrap(),
            Target::Range {
                base: "main".into(),
                head: "HEAD".into()
            }
        );
        assert!(Target::parse(Some("feature/x")).is_err());
        assert_eq!(
            Target::parse(Some("WORKTREE")).unwrap(),
            Target::Range {
                base: "HEAD".into(),
                head: WORKTREE.into()
            }
        );
        assert_eq!(
            Target::parse(Some("main..INDEX")).unwrap(),
            Target::Range {
                base: "main".into(),
                head: INDEX.into()
            }
        );
    }

    #[test]
    fn opens_the_uncommitted_changes() {
        let tmp = tempfile::tempdir().unwrap();
        let g = repo(tmp.path());
        std::fs::write(tmp.path().join(".gitignore"), "target/\n").unwrap();
        let base = commit(&g, &[("a.txt", "1\n"), ("b.txt", "b\n")], "base");
        g.bytes(&["checkout", "--quiet", "-b", "feat"]).unwrap();
        commit(&g, &[("c.txt", "committed\n")], "feat");
        let head = g.rev_parse("HEAD").unwrap();
        // Staged, then modified again; an untracked file; an ignored one.
        std::fs::write(tmp.path().join("a.txt"), "2\n").unwrap();
        g.bytes(&["add", "a.txt"]).unwrap();
        std::fs::write(tmp.path().join("a.txt"), "3\n").unwrap();
        std::fs::write(tmp.path().join("new.txt"), "new\n").unwrap();
        std::fs::create_dir(tmp.path().join("target")).unwrap();
        std::fs::write(tmp.path().join("target/out"), "x\n").unwrap();
        let status = g.text(&["status", "--porcelain"]).unwrap();

        let open_ = |t: &str| {
            open(
                &g,
                &Config::default(),
                &Target::parse(Some(t)).unwrap(),
                |_| {},
            )
            .unwrap()
        };
        let paths =
            |r: &Review| -> Vec<String> { r.diff.files.iter().map(|f| f.path.clone()).collect() };
        let new_text = |r: &Review, path: &str| read_file(&r.repo, &r.head_sha, path);

        let wt = open_("WORKTREE");
        assert_eq!(wt.base_sha, head);
        assert_eq!(paths(&wt), ["a.txt", "new.txt"]);
        assert_eq!(new_text(&wt, "a.txt").as_deref(), Some("3\n"));
        assert_eq!(wt.worktree, g.dir());
        assert_eq!(wt.title(), format!("{}..working tree", short(&head)));
        // Same files, same snapshot: the caches keyed by the head still apply.
        assert_eq!(open_("WORKTREE").head_sha, wt.head_sha);

        let idx = open_("INDEX");
        assert_eq!(paths(&idx), ["a.txt"]);
        assert_eq!(new_text(&idx, "a.txt").as_deref(), Some("2\n"));
        assert_ne!(idx.worktree, g.dir());

        let branch = open_("main..WORKTREE");
        assert_eq!(branch.base_sha, base);
        assert_eq!(paths(&branch), ["a.txt", "c.txt", "new.txt"]);

        // Nothing moved: index, HEAD, status.
        assert_eq!(g.rev_parse("HEAD").unwrap(), head);
        assert_eq!(g.text(&["status", "--porcelain"]).unwrap(), status);
    }

    #[test]
    fn opens_local_range_from_merge_base() {
        let tmp = tempfile::tempdir().unwrap();
        let g = repo(tmp.path());
        let base = commit(&g, &[("a.txt", "1\n")], "base");
        g.bytes(&["checkout", "--quiet", "-b", "feat"]).unwrap();
        commit(
            &g,
            &[("b.txt", "feat\n"), ("package-lock.json", "{}\n")],
            "feat",
        );
        g.bytes(&["checkout", "--quiet", "main"]).unwrap();
        commit(&g, &[("a.txt", "2\n")], "main moves on");

        let r = open(
            &g,
            &Config::default(),
            &Target::parse(Some("main..feat")).unwrap(),
            |_| {},
        )
        .unwrap();
        assert_eq!(r.base_sha, base);
        let paths: Vec<_> = r.diff.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, vec!["b.txt", "package-lock.json"]);
        assert!(r.diff.files[1].is_generated);
        assert_ne!(r.worktree, g.dir());
        r.ensure_worktree().unwrap();
        assert!(r.worktree.join("b.txt").exists());
    }

    #[test]
    fn builds_and_caches_the_graph() {
        let tmp = tempfile::tempdir().unwrap();
        let g = repo(tmp.path());
        let svc = "package app;\nclass Svc {\n  int total(int a) {\n    return a;\n  }\n}\n";
        let user =
            "package app;\nclass User {\n  Svc svc;\n  int go() { return svc.total(1); }\n}\n";
        commit(
            &g,
            &[("src/Svc.java", svc), ("src/User.java", user)],
            "base",
        );
        g.bytes(&["checkout", "--quiet", "-b", "feat"]).unwrap();
        commit(
            &g,
            &[("src/Svc.java", &svc.replace("return a;", "return a + 1;"))],
            "feat",
        );

        let r = open(
            &g,
            &Config::default(),
            &Target::parse(Some("main..feat")).unwrap(),
            |_| {},
        )
        .unwrap();
        let graph = build_graph(&r, &Config::default(), |_| {}, true).unwrap();
        assert!(!graph.from_cache);
        let changed: Vec<_> = graph
            .changed_symbols()
            .iter()
            .map(|&s| graph.display_name(s))
            .collect();
        assert_eq!(changed, ["Svc.total"]);
        let callers = graph.callers(graph.changed_symbols()[0]);
        assert_eq!(graph.display_name(callers[0].symbol), "User.go");
        assert!(!graph.is_file_changed("src/User.java"));

        let again = build_graph(&r, &Config::default(), |_| {}, true).unwrap();
        assert!(again.from_cache);
        assert_eq!(again.symbols(), graph.symbols());

        // Language servers: a missing one leaves the graph as it was and
        // caches nothing; disabled, nothing runs.
        let mut cfg = Config::default();
        cfg.lsp.java.command = Some("/nonexistent/jdtls".into());
        let refined = refine_graph(&r, &cfg, &graph, &|_| {}, true)
            .unwrap()
            .unwrap();
        assert_eq!(refined.edges(), graph.edges());
        let stats = refined.lsp_stats().unwrap();
        assert_eq!(stats.servers[0].status, "missing");
        assert!(!lsp::cache_path(&g.survol_dir().unwrap(), &r.head_sha).exists());
        cfg.lsp.enabled = false;
        assert!(
            refine_graph(&r, &cfg, &graph, &|_| {}, true)
                .unwrap()
                .is_none()
        );
    }
}
