//! `survol-cli`: the engine behind a JSON interface, for tests, debugging and scripts.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use serde_json::json;
use survol_core::config::Config;
use survol_core::doctor::{self, Status};
use survol_core::git::Git;
use survol_core::llm::ClaudeCli;
use survol_core::review::{self, Target};

#[derive(Parser)]
#[command(
    version,
    about = "survol engine: JSON commands for tests, debugging and scripts"
)]
struct Cli {
    /// Repository to work in (defaults to the current directory).
    #[arg(short = 'C', long, global = true)]
    repo: Option<PathBuf>,
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Check git, GitLab access, nvim and the LLM CLI.
    Doctor {
        /// Output JSON instead of a table.
        #[arg(long)]
        json: bool,
    },
    /// Print the effective configuration and where it is read from.
    Config,
    /// Fetch a merge request and check it out in its worktree.
    Fetch {
        /// MR number, `!number`, MR URL, or `base..head`. Empty: MR of the current branch.
        target: Option<String>,
    },
    /// Print the parsed diff of a merge request or range as JSON.
    Diff {
        /// MR number, `!number`, MR URL, or `base..head`. Empty: MR of the current branch.
        target: Option<String>,
    },
    /// Group the hunks by functional capability and layer (Stack view), as JSON.
    Group {
        /// MR number, `!number`, MR URL, or `base..head`. Empty: MR of the current branch.
        target: Option<String>,
        /// Ignore the cached grouping and call the LLM again.
        #[arg(long)]
        no_cache: bool,
    },
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<ExitCode> {
    let cli = Cli::parse();
    let cwd = match cli.repo {
        Some(p) => p,
        None => std::env::current_dir()?,
    };
    let repo = Git::discover(&cwd);
    let cfg = Config::load(repo.as_ref().map(Git::dir))?;

    match cli.command {
        Cmd::Doctor { json } => {
            let checks = doctor::run(&cwd, &cfg);
            if json {
                println!("{}", serde_json::to_string_pretty(&checks)?);
            } else {
                for c in &checks {
                    let mark = match c.status {
                        Status::Ok => "ok  ",
                        Status::Warn => "warn",
                        Status::Fail => "FAIL",
                    };
                    println!("{mark}  {:<15} {}", c.name, c.detail);
                }
            }
            let failed = checks.iter().any(|c| c.status == Status::Fail);
            return Ok(if failed {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            });
        }
        Cmd::Config => {
            let paths = json!({
                "user": Config::user_path(),
                "project": repo.as_ref().map(|r| Config::project_path(r.dir())),
            });
            println!("# files: {paths}");
            print!("{}", toml::to_string_pretty(&cfg)?);
        }
        Cmd::Fetch { target } => {
            let repo = repo.context("not inside a git repository")?;
            let r = review::open(&repo, &cfg, &Target::parse(target.as_deref())?, progress)?;
            progress("checking out worktree");
            r.ensure_worktree()?;
            let out = json!({
                "merge_request": r.mr,
                "base_sha": r.base_sha,
                "head_sha": r.head_sha,
                "worktree": r.worktree,
                "files": r.diff.files.len(),
                "hunks": r.diff.hunks.len(),
            });
            println!("{}", serde_json::to_string_pretty(&out)?);
        }
        Cmd::Diff { target } => {
            let repo = repo.context("not inside a git repository")?;
            let r = review::open(&repo, &cfg, &Target::parse(target.as_deref())?, progress)?;
            let out = json!({
                "merge_request": r.mr,
                "base_sha": r.base_sha,
                "head_sha": r.head_sha,
                "files": r.diff.files,
                "hunks": r.diff.hunks,
            });
            println!("{}", serde_json::to_string(&out)?);
        }
        Cmd::Group { target, no_cache } => {
            let repo = repo.context("not inside a git repository")?;
            let r = review::open(&repo, &cfg, &Target::parse(target.as_deref())?, progress)?;
            let llm = ClaudeCli::from_config(&cfg.llm);
            let started = Instant::now();
            let grouping = review::group(&r, &cfg, &llm, progress, !no_cache)?;
            progress(&format!(
                "{} groups ({:?}, {} LLM call(s), {:.1}s)",
                grouping.groups.len(),
                grouping.source,
                grouping.llm_calls,
                started.elapsed().as_secs_f64()
            ));
            for w in &grouping.warnings {
                eprintln!("warning: {w}");
            }
            println!("{}", serde_json::to_string_pretty(&grouping)?);
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn progress(msg: &str) {
    eprintln!("· {msg}");
}
