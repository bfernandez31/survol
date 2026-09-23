//! Opening a review: resolve the target, fetch, diff, and locate the worktree.

use std::path::{Path, PathBuf};

use crate::config::Config;
use crate::forge::gitlab::Gitlab;
use crate::forge::{Forge, ForgeError, MrRef, project_from_remote};
use crate::git::Git;
use crate::graph::{self, Graph};
use crate::group::{self, Grouping};
use crate::index::{self, Index};
use crate::llm::LlmProvider;
use crate::model::{Diff, FileStatus, MergeRequest};
use crate::{Error, Result, diff, mechanical};

/// What to review.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// A merge request; `None` means the one of the current branch.
    MergeRequest(Option<MrRef>),
    /// A local range `base..head`, diffed from their merge base like GitLab.
    Range { base: String, head: String },
}

impl Target {
    /// `123`, `!123`, a MR URL, `base..head`, or nothing.
    pub fn parse(arg: Option<&str>) -> Result<Self> {
        let Some(arg) = arg.map(str::trim).filter(|a| !a.is_empty()) else {
            return Ok(Self::MergeRequest(None));
        };
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
            "`{arg}` is neither a merge request (number or URL) nor a range base..head"
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
    /// Where the head is checked out. Created by [`Review::ensure_worktree`].
    pub worktree: PathBuf,
    /// Key of the review state, see [`crate::review_state::state_path`].
    pub state_key: String,
    pub repo: Git,
}

impl Review {
    pub fn title(&self) -> String {
        match &self.mr {
            Some(mr) => format!("!{} {}", mr.iid, mr.title),
            None => format!("{}..{}", short(&self.base_sha), short(&self.head_sha)),
        }
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
    let (mr, base_sha, head_sha, state_key, worktree) = match target {
        Target::MergeRequest(mr_ref) => {
            progress("fetching merge request metadata");
            let forge = Gitlab::from_config(&cfg.gitlab)?;
            let mr = resolve_mr(repo, cfg, &forge, mr_ref.as_ref())?;
            fetch_shas(repo, cfg, &mr, &mut progress)?;
            let key = format!("mr-{}", mr.iid);
            let wt = repo.survol_dir()?.join("worktrees").join(&key);
            (Some(mr.clone()), mr.base_sha, mr.head_sha, key, wt)
        }
        Target::Range { base, head } => {
            let head_sha = repo.rev_parse(head)?;
            let base_sha = repo.text(&["merge-base", &repo.rev_parse(base)?, &head_sha])?;
            let key = format!("range-{}", sanitize(&format!("{base}..{head}")));
            // Reviewing the checked-out commit: the repo itself is the worktree.
            let wt = if repo.rev_parse("HEAD").ok().as_deref() == Some(head_sha.as_str()) {
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
    let cwd = if review.worktree.is_dir() {
        review.worktree.clone()
    } else {
        review.repo.dir().to_path_buf()
    };
    let params = group::Params {
        model: cfg.llm.group_model.clone(),
        effort: cfg.llm.group_effort.clone(),
        max_prompt_chars: cfg.llm.max_prompt_chars,
        instructions: read_instructions(review.repo.dir())?,
        language: cfg.llm.language(),
        cwd,
    };
    if !cfg.llm.enabled {
        progress("LLM disabled: grouping by directory");
        return Ok(group::build_offline(&review.diff, &params));
    }
    let path = group::cache_path(&review.repo.survol_dir()?, &review.head_sha);
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

/// `.survol/instructions.md`: the team's architecture conventions, if any.
pub fn instructions_path(repo_root: &Path) -> PathBuf {
    repo_root.join(".survol/instructions.md")
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
        repo.fetch_merge_request(remote, mr.iid)?;
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
            "head {} of !{} is not reachable after fetch",
            short(&mr.head_sha),
            mr.iid
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
    }
}
