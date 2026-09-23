//! Thin wrapper over the `git` binary.
//!
//! Shelling out keeps behaviour identical to the user's command line: same
//! credentials, SSH config, `refs/merge-requests/*` support and rename detection.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

#[derive(Debug, thiserror::Error)]
pub enum GitError {
    #[error("could not run git: {0}")]
    Spawn(#[from] std::io::Error),
    #[error("`git {args}` failed: {stderr}")]
    Failed { args: String, stderr: String },
}

pub type Result<T> = std::result::Result<T, GitError>;

#[derive(Debug, Clone)]
pub struct Git {
    dir: PathBuf,
}

impl Git {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// Repository containing `dir`, or `None` when it is not inside one.
    pub fn discover(dir: &Path) -> Option<Self> {
        let git = Self::new(dir);
        let top = git.text(&["rev-parse", "--show-toplevel"]).ok()?;
        Some(Self::new(top))
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn command(&self) -> Command {
        let mut cmd = Command::new("git");
        cmd.current_dir(&self.dir)
            .env("GIT_TERMINAL_PROMPT", "0")
            .args(["-c", "core.quotePath=false"]);
        cmd
    }

    fn check(args: &[&str], out: Output) -> Result<Vec<u8>> {
        if out.status.success() {
            Ok(out.stdout)
        } else {
            Err(GitError::Failed {
                args: args.join(" "),
                stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
            })
        }
    }

    pub fn bytes(&self, args: &[&str]) -> Result<Vec<u8>> {
        Self::check(args, self.command().args(args).output()?)
    }

    pub fn text(&self, args: &[&str]) -> Result<String> {
        Ok(String::from_utf8_lossy(&self.bytes(args)?)
            .trim()
            .to_string())
    }

    pub fn version() -> Result<String> {
        let out = Command::new("git").arg("--version").output()?;
        Ok(String::from_utf8_lossy(&Self::check(&["--version"], out)?)
            .trim()
            .to_string())
    }

    /// Shared `.git` directory (the main one, even from a worktree).
    pub fn common_dir(&self) -> Result<PathBuf> {
        let p = PathBuf::from(self.text(&["rev-parse", "--git-common-dir"])?);
        Ok(if p.is_absolute() { p } else { self.dir.join(p) })
    }

    /// Directory where survol keeps its state and caches: `.git/survol`.
    pub fn survol_dir(&self) -> Result<PathBuf> {
        Ok(self.common_dir()?.join("survol"))
    }

    pub fn current_branch(&self) -> Result<Option<String>> {
        let b = self.text(&["branch", "--show-current"])?;
        Ok((!b.is_empty()).then_some(b))
    }

    pub fn remote_url(&self, remote: &str) -> Result<String> {
        self.text(&["remote", "get-url", remote])
    }

    pub fn has_commit(&self, sha: &str) -> bool {
        self.bytes(&["cat-file", "-e", &format!("{sha}^{{commit}}")])
            .is_ok()
    }

    pub fn rev_parse(&self, rev: &str) -> Result<String> {
        self.text(&["rev-parse", "--verify", &format!("{rev}^{{commit}}")])
    }

    pub fn fetch(&self, remote: &str, refspecs: &[&str]) -> Result<()> {
        let mut args = vec!["fetch", "--quiet", "--no-tags", remote];
        args.extend_from_slice(refspecs);
        self.bytes(&args).map(drop)
    }

    /// Fetches the head of merge request `iid` into `refs/survol/mr/<iid>`.
    pub fn fetch_merge_request(&self, remote: &str, iid: u64) -> Result<()> {
        let spec = format!("+refs/merge-requests/{iid}/head:refs/survol/mr/{iid}");
        self.fetch(remote, &[&spec])
    }

    /// Raw `git diff -M base head`, in the format expected by [`crate::diff::parse`].
    pub fn diff(&self, base: &str, head: &str) -> Result<Vec<u8>> {
        self.bytes(&[
            "diff",
            "-M",
            "--no-color",
            "--no-ext-diff",
            "--no-textconv",
            "--src-prefix=a/",
            "--dst-prefix=b/",
            "--diff-algorithm=histogram",
            base,
            head,
        ])
    }

    /// Creates, or moves, a detached worktree at `path` on `sha`.
    pub fn ensure_worktree(&self, path: &Path, sha: &str) -> Result<()> {
        let p = path.to_string_lossy();
        if path.join(".git").exists() {
            let wt = Git::new(path);
            if wt.rev_parse("HEAD").ok().as_deref() != Some(sha) {
                wt.bytes(&["checkout", "--quiet", "--force", "--detach", sha])?;
            }
            return Ok(());
        }
        // A stale entry would make `worktree add` refuse the path.
        let _ = self.bytes(&["worktree", "prune"]);
        self.bytes(&["worktree", "add", "--quiet", "--force", "--detach", &p, sha])
            .map(drop)
    }
}

#[cfg(test)]
pub(crate) mod testutil {
    use super::Git;
    use std::path::Path;

    pub fn repo(dir: &Path) -> Git {
        let g = Git::new(dir);
        g.bytes(&["init", "--quiet", "-b", "main"]).unwrap();
        g.bytes(&["config", "user.email", "t@example.com"]).unwrap();
        g.bytes(&["config", "user.name", "t"]).unwrap();
        g.bytes(&["config", "commit.gpgsign", "false"]).unwrap();
        g
    }

    pub fn commit(g: &Git, files: &[(&str, &str)], msg: &str) -> String {
        for (path, content) in files {
            let p = g.dir().join(path);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, content).unwrap();
        }
        g.bytes(&["add", "-A"]).unwrap();
        g.bytes(&["commit", "--quiet", "-m", msg]).unwrap();
        g.rev_parse("HEAD").unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::testutil::*;

    #[test]
    fn diff_and_worktree() {
        let tmp = tempfile::tempdir().unwrap();
        let g = repo(tmp.path());
        let base = commit(&g, &[("a.txt", "one\ntwo\n"), ("b.txt", "keep\n")], "base");
        std::fs::rename(tmp.path().join("b.txt"), tmp.path().join("c.txt")).unwrap();
        let head = commit(&g, &[("a.txt", "one\n2\n")], "head");

        let d = crate::diff::parse(&g.diff(&base, &head).unwrap()).unwrap();
        assert_eq!(d.files.len(), 2);
        assert_eq!(d.files[1].old_path.as_deref(), Some("b.txt"));

        let wt = g.survol_dir().unwrap().join("worktrees/test");
        g.ensure_worktree(&wt, &base).unwrap();
        assert_eq!(
            std::fs::read_to_string(wt.join("a.txt")).unwrap(),
            "one\ntwo\n"
        );
        g.ensure_worktree(&wt, &head).unwrap();
        assert_eq!(
            std::fs::read_to_string(wt.join("a.txt")).unwrap(),
            "one\n2\n"
        );
    }
}
