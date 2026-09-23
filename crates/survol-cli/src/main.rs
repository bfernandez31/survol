//! `survol-cli`: the engine behind a JSON interface, for tests, debugging and scripts.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use serde_json::json;
use survol_core::ask::Subject;
use survol_core::config::Config;
use survol_core::doctor::{self, Status};
use survol_core::git::Git;
use survol_core::graph::{Graph, Link, SymIdx, SymbolKind};
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
        /// Do not call the LLM: group by directory (same as `[llm] enabled = false`).
        #[arg(long)]
        no_llm: bool,
        /// Language of the titles and summaries: a name or a code (`fr`,
        /// `français`, `en`...). Overrides `[llm] language`.
        #[arg(long, value_name = "LANG")]
        lang: Option<String>,
    },
    /// Code graph (tree-sitter): changed symbols with their callers, callees
    /// and tests, as JSON.
    Graph {
        /// MR number, `!number`, MR URL, or `base..head`. Empty: MR of the current branch.
        target: Option<String>,
        /// Only the symbols with this name (`find`, or `OwnerService.find`),
        /// changed or not.
        #[arg(long, value_name = "NAME")]
        symbol: Option<String>,
        /// Print the module map (package / directory dependencies) instead.
        #[arg(long)]
        modules: bool,
        /// Print the module map as a Mermaid flowchart instead.
        #[arg(long, conflicts_with_all = ["symbol", "modules"])]
        mermaid: bool,
        /// Rebuild the graph instead of loading it from the cache.
        #[arg(long)]
        no_cache: bool,
    },
    /// Ask the LLM a question about a symbol, a Stack group or a hunk. The
    /// answer cites code as `[path:line]`, checked against the context.
    Ask {
        /// `[TARGET] QUESTION`: the target as for the other commands (empty:
        /// MR of the current branch), then the question.
        #[arg(num_args = 1..=2, required = true, value_name = "[TARGET] QUESTION")]
        args: Vec<String>,
        /// A symbol by name (`find`, `OwnerService.find`).
        #[arg(long, value_name = "NAME", conflicts_with_all = ["group", "hunk"])]
        symbol: Option<String>,
        /// A group of the Stack view, numbered as displayed (1 = first).
        #[arg(long, value_name = "N", conflicts_with = "hunk")]
        group: Option<usize>,
        /// A hunk, by its id in `survol-cli diff`.
        #[arg(long, value_name = "ID")]
        hunk: Option<usize>,
        /// Ask again instead of reusing a cached answer.
        #[arg(long)]
        no_cache: bool,
        /// Print the prompt instead of asking (no LLM call).
        #[arg(long)]
        prompt: bool,
        /// Language of the answer: a name or a code. Overrides `[llm] language`.
        #[arg(long, value_name = "LANG")]
        lang: Option<String>,
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
        Cmd::Group {
            target,
            no_cache,
            no_llm,
            lang,
        } => {
            let mut cfg = cfg;
            cfg.llm.enabled &= !no_llm;
            cfg.llm.override_language(lang.as_deref());
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
        Cmd::Graph {
            target,
            symbol,
            modules,
            mermaid,
            no_cache,
        } => {
            let repo = repo.context("not inside a git repository")?;
            let r = review::open(&repo, &cfg, &Target::parse(target.as_deref())?, progress)?;
            let started = Instant::now();
            let g = review::build_graph(&r, &cfg, progress, !no_cache)?;
            progress(&format!(
                "graph ready in {:.2}s",
                started.elapsed().as_secs_f64()
            ));
            if mermaid {
                print!("{}", g.module_map().to_mermaid());
                return Ok(ExitCode::SUCCESS);
            }
            let out = if modules {
                let map = g.module_map();
                json!({
                    "modules": map.modules,
                    "edges": map.edges.iter().map(|e| json!({
                        "from": map.modules[e.from].name,
                        "to": map.modules[e.to].name,
                        "count": e.count,
                        "kinds": e.kinds,
                    })).collect::<Vec<_>>(),
                })
            } else {
                let syms: Vec<SymIdx> = match &symbol {
                    Some(name) => g.find(name),
                    None => g
                        .changed_symbols()
                        .into_iter()
                        .filter(|&s| g.symbol(s).kind != SymbolKind::File)
                        .collect(),
                };
                json!({
                    "base_sha": r.base_sha,
                    "head_sha": r.head_sha,
                    "stats": g.stats(),
                    "symbols": syms.iter().map(|&s| symbol_json(&g, s)).collect::<Vec<_>>(),
                })
            };
            println!("{}", serde_json::to_string_pretty(&out)?);
        }
        Cmd::Ask {
            args,
            symbol,
            group,
            hunk,
            no_cache,
            prompt,
            lang,
        } => ask(repo, cfg, args, symbol, group, hunk, no_cache, prompt, lang)?,
    }
    Ok(ExitCode::SUCCESS)
}

#[allow(clippy::too_many_arguments)]
fn ask(
    repo: Option<Git>,
    mut cfg: Config,
    args: Vec<String>,
    symbol: Option<String>,
    group: Option<usize>,
    hunk: Option<usize>,
    no_cache: bool,
    print_prompt: bool,
    lang: Option<String>,
) -> Result<()> {
    cfg.llm.override_language(lang.as_deref());
    if !cfg.llm.enabled && !print_prompt {
        anyhow::bail!("the LLM is disabled ([llm] enabled = false)");
    }
    let (target, question) = match args.as_slice() {
        [q] => (None, q.clone()),
        [t, q] => (Some(t.as_str()), q.clone()),
        _ => anyhow::bail!("expected [TARGET] QUESTION"),
    };
    let repo = repo.context("not inside a git repository")?;
    let r = review::open(&repo, &cfg, &Target::parse(target)?, progress)?;
    let graph = review::build_graph(&r, &cfg, progress, true)?;
    let llm = ClaudeCli::from_config(&cfg.llm);
    let (subject, grouping) = match (symbol, group, hunk) {
        (Some(name), _, _) => {
            let mut hits: Vec<SymIdx> = graph
                .find(&name)
                .into_iter()
                .filter(|&s| graph.symbol(s).kind != SymbolKind::File)
                .collect();
            if hits.len() > 1 && hits.iter().any(|&s| graph.symbol(s).changed) {
                hits.retain(|&s| graph.symbol(s).changed);
            }
            match hits.as_slice() {
                [] => anyhow::bail!("no symbol named `{name}`"),
                [s] => (Subject::Symbol(graph.symbol(*s).id.clone()), None),
                many => anyhow::bail!(
                    "`{name}` is ambiguous: {}",
                    many.iter()
                        .map(|&s| graph.symbol(s).id.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            }
        }
        (None, Some(n), _) => {
            let mut g = review::group(&r, &cfg, &llm, progress, true)?;
            g.order_with_graph(&graph);
            let id = n
                .checked_sub(1)
                .and_then(|i| g.groups.get(i))
                .map(|gr| gr.id)
                .with_context(|| format!("no group {n} (there are {})", g.groups.len()))?;
            (Subject::Group(id), Some(g))
        }
        (None, None, Some(h)) => {
            let hunk = r
                .diff
                .hunks
                .get(h)
                .with_context(|| format!("no hunk {h} (there are {})", r.diff.hunks.len()))?;
            (Subject::Hunk(hunk.content_hash.clone()), None)
        }
        (None, None, None) => {
            anyhow::bail!("say what the question is about: --symbol, --group or --hunk")
        }
    };
    // The group summary gives context to symbol and hunk questions too.
    let grouping = match grouping {
        Some(g) => Some(g),
        None if cfg.llm.enabled => {
            let path = survol_core::group::cache_path(&repo.survol_dir()?, &r.head_sha);
            std::fs::read(path)
                .ok()
                .and_then(|b| serde_json::from_slice::<survol_core::group::Grouping>(&b).ok())
        }
        None => None,
    };
    let prompt = review::ask_prompt(
        &r,
        &cfg,
        Some(&graph),
        grouping.as_ref(),
        &subject,
        &question,
    )?;
    if print_prompt {
        print!("{}", prompt.text);
        return Ok(());
    }
    progress(&format!(
        "asking about {} ({} chars of context)",
        prompt.label,
        prompt.text.len()
    ));
    let started = Instant::now();
    let answer = survol_core::ask::ask(&prompt, &llm, &review::ask_params(&r, &cfg, !no_cache)?)?;
    progress(&format!(
        "answered in {:.1}s{}, {} reference(s), {} unknown",
        started.elapsed().as_secs_f64(),
        if answer.from_cache { " (cached)" } else { "" },
        answer.refs.len(),
        answer.unknown_refs()
    ));
    survol_core::ask::append_history(&review::questions_path(&r)?, &answer)?;
    println!("{}", serde_json::to_string_pretty(&answer)?);
    Ok(())
}

/// A symbol with its callers, callees and tests.
fn symbol_json(g: &Graph, s: SymIdx) -> serde_json::Value {
    let sym = g.symbol(s);
    json!({
        "id": sym.id,
        "name": g.display_name(s),
        "kind": sym.kind,
        "file": sym.file,
        "line": sym.line,
        "changed": sym.changed,
        "removed": sym.removed,
        "roles": sym.roles,
        "annotations": sym.annotations,
        "hunks": g.hunks_of_symbol(s),
        "callers": links_json(g, &g.callers(s), true),
        "callees": links_json(g, &g.callees(s), false),
        "tests": links_json(g, &g.tests_of(s), true),
    })
}

/// `at_reference`: `line` is where the link's symbol makes the reference
/// (callers, tests), else where it is defined (callees).
fn links_json(g: &Graph, links: &[Link], at_reference: bool) -> Vec<serde_json::Value> {
    links
        .iter()
        .map(|l| {
            let other = g.symbol(l.symbol);
            json!({
                "id": other.id,
                "name": g.display_name(l.symbol),
                "file": other.file,
                "line": if at_reference && l.line > 0 { l.line } else { other.line },
                "file_changed": g.is_file_changed(&other.file),
                "symbol_changed": other.changed,
                "confidence": (f64::from(l.confidence) * 100.0).round() / 100.0,
                "via": l.via.map(|v| g.symbol(v).id.clone()),
            })
        })
        .collect()
}

fn progress(msg: &str) {
    eprintln!("· {msg}");
}
