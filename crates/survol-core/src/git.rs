//! Thin wrapper over the `git` binary.
//!
//! Shelling out keeps behaviour identical to the user's command line: same
//! credentials, SSH config, `refs/merge-requests/*` / `refs/pull/*` support
//! and rename detection.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use crate::forge::ForgeKind;

/// The server-side ref of a merge / pull request's head.
pub fn head_ref(forge: ForgeKind, iid: u64) -> String {
    match forge {
        ForgeKind::Gitlab => format!("refs/merge-requests/{iid}/head"),
        ForgeKind::Github => format!("refs/pull/{iid}/head"),
    }
}

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

    /// Fetches the head of merge request `iid` into `refs/survol/mr/<iid>`:
    /// `refs/merge-requests/<iid>/head` on GitLab, `refs/pull/<n>/head` on
    /// GitHub.
    pub fn fetch_merge_request(&self, remote: &str, forge: ForgeKind, iid: u64) -> Result<()> {
        let spec = format!("+{}:refs/survol/mr/{iid}", head_ref(forge, iid));
        self.fetch(remote, &[&spec])
    }

    pub fn merge_base(&self, a: &str, b: &str) -> Result<String> {
        self.text(&["merge-base", a, b])
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

    /// Every blob of the tree of `rev` (`git ls-tree -r -l`): files only, no
    /// symlinks or submodules. Works without a checkout.
    pub fn ls_tree(&self, rev: &str) -> Result<Vec<TreeEntry>> {
        let out = self.bytes(&["ls-tree", "-r", "-l", "-z", "--full-tree", rev])?;
        Ok(out
            .split(|&b| b == 0)
            .filter_map(|rec| {
                // `<mode> SP <type> SP <id> SP+ <size> TAB <path>`
                let rec = std::str::from_utf8(rec).ok()?;
                let (meta, path) = rec.split_once('\t')?;
                let mut it = meta.split_ascii_whitespace();
                let (mode, kind, id, size) = (it.next()?, it.next()?, it.next()?, it.next()?);
                (kind == "blob" && mode != "120000").then(|| TreeEntry {
                    path: path.to_string(),
                    blob: id.to_string(),
                    size: size.parse().unwrap_or(0),
                })
            })
            .collect())
    }

    /// Contents of the blobs `ids`, in order, through one `git cat-file --batch`.
    pub fn read_blobs(&self, ids: &[&str]) -> Result<Vec<Vec<u8>>> {
        use std::io::{BufRead, BufReader, Read, Write};
        use std::process::Stdio;

        let args = ["cat-file", "--batch"];
        let mut child = self
            .command()
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let mut stdin = child.stdin.take().expect("piped stdin");
        let input: String = ids.iter().map(|id| format!("{id}\n")).collect();
        // Written from another thread: git blocks on a full stdout otherwise.
        let writer = std::thread::spawn(move || stdin.write_all(input.as_bytes()));
        let mut out = BufReader::new(child.stdout.take().expect("piped stdout"));
        let mut blobs = Vec::with_capacity(ids.len());
        let mut header = String::new();
        for id in ids {
            header.clear();
            out.read_line(&mut header)?;
            // `<id> blob <size>`, or `<id> missing`.
            let size = header
                .trim_end()
                .rsplit_once(' ')
                .and_then(|(_, s)| s.parse::<usize>().ok())
                .filter(|_| !header.ends_with("missing\n"))
                .ok_or_else(|| GitError::Failed {
                    args: args.join(" "),
                    stderr: format!("cannot read blob {id}: {}", header.trim()),
                })?;
            let mut buf = vec![0; size + 1]; // content + trailing LF
            out.read_exact(&mut buf)?;
            buf.pop();
            blobs.push(buf);
        }
        let _ = writer.join();
        let _ = child.wait();
        Ok(blobs)
    }
}

/// A file of a git tree, see [`Git::ls_tree`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeEntry {
    pub path: String,
    /// Blob id: identifies the content, whatever the path or commit.
    pub blob: String,
    pub size: u64,
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

    #[test]
    fn fetches_pull_and_merge_request_heads() {
        use crate::forge::ForgeKind;
        let tmp = tempfile::tempdir().unwrap();
        for d in ["server", "local"] {
            std::fs::create_dir_all(tmp.path().join(d)).unwrap();
        }
        let server = repo(&tmp.path().join("server"));
        let base = commit(&server, &[("a.txt", "1\n")], "base");
        let head = commit(&server, &[("a.txt", "2\n")], "head");
        server
            .bytes(&["update-ref", "refs/pull/7/head", &head])
            .unwrap();
        server
            .bytes(&["update-ref", "refs/merge-requests/8/head", &head])
            .unwrap();
        server
            .bytes(&["reset", "--quiet", "--hard", &base])
            .unwrap();

        let local = repo(&tmp.path().join("local"));
        let remote = tmp.path().join("server").display().to_string();
        local.bytes(&["remote", "add", "origin", &remote]).unwrap();
        local.fetch("origin", &["main"]).unwrap();
        assert!(!local.has_commit(&head));
        local
            .fetch_merge_request("origin", ForgeKind::Github, 7)
            .unwrap();
        assert_eq!(local.rev_parse("refs/survol/mr/7").unwrap(), head);
        local
            .fetch_merge_request("origin", ForgeKind::Gitlab, 8)
            .unwrap();
        assert_eq!(local.rev_parse("refs/survol/mr/8").unwrap(), head);
        assert_eq!(local.merge_base(&base, &head).unwrap(), base);
    }

    #[test]
    fn lists_tree_and_reads_blobs() {
        let tmp = tempfile::tempdir().unwrap();
        let g = repo(tmp.path());
        let head = commit(&g, &[("a.txt", "one\n"), ("d/b.txt", "")], "base");
        let tree = g.ls_tree(&head).unwrap();
        let paths: Vec<_> = tree.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(paths, ["a.txt", "d/b.txt"]);
        assert_eq!(tree[0].size, 4);
        let ids: Vec<_> = tree.iter().map(|e| e.blob.as_str()).collect();
        let blobs = g.read_blobs(&ids).unwrap();
        assert_eq!(blobs, [b"one\n".to_vec(), Vec::new()]);
        assert!(
            g.read_blobs(&["0000000000000000000000000000000000000000"])
                .is_err()
        );
    }
}
