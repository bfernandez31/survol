//! `survol`: a bird's-eye view of large merge requests, in the terminal.

mod app;
mod editor;
mod highlight;
mod ui;
mod views;

use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::Parser;
use crossterm::event::{self, Event, KeyEventKind};
use survol_core::config::Config;
use survol_core::git::Git;
use survol_core::review::{self, Target};
use survol_core::review_state::ReviewState;

use crate::app::{App, GraphStatus};

#[derive(Parser)]
#[command(
    version,
    about = "A bird's-eye view of large pull requests",
    long_about = "A bird's-eye view of large pull requests.\n\n\
        Opens a GitLab merge request or a GitHub pull request (or a local range) \
        in a terminal review UI. \
        Run `survol-cli doctor` to check your setup."
)]
struct Cli {
    /// Merge / pull request number (`123`, `!123`, `#123`), its URL, or local
    /// range `base..head`. Empty: the merge / pull request of the current branch.
    target: Option<String>,
    /// Repository to work in (defaults to the current directory).
    #[arg(short = 'C', long)]
    repo: Option<PathBuf>,
    /// Never call the LLM: the Stack view groups by directory
    /// (same as `[llm] enabled = false`).
    #[arg(long)]
    no_llm: bool,
    /// Language of the LLM-written titles and summaries: a name or a code
    /// (`fr`, `français`, `en`...). Overrides `[llm] language`.
    #[arg(long, value_name = "LANG")]
    lang: Option<String>,
    /// Keep the heuristic graph: do not start language servers
    /// (same as `[lsp] enabled = false`).
    #[arg(long)]
    no_lsp: bool,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let cwd = match cli.repo {
        Some(p) => p,
        None => std::env::current_dir()?,
    };
    let Some(repo) = Git::discover(&cwd) else {
        bail!("{} is not inside a git repository", cwd.display());
    };
    let mut cfg = Config::load(Some(repo.dir()))?;
    cfg.llm.enabled &= !cli.no_llm;
    cfg.lsp.enabled &= !cli.no_lsp;
    cfg.llm.override_language(cli.lang.as_deref());
    let target = Target::parse(cli.target.as_deref())?;

    let review = review::open(&repo, &cfg, &target, |m| eprintln!("· {m}"))?;
    if review.diff.files.is_empty() {
        println!("{}: no changes", review.title());
        return Ok(());
    }
    let state_path = review.state_path()?;
    let state = ReviewState::load(&state_path)
        .with_context(|| format!("reading {}", state_path.display()))?;

    // The worktree is only needed to open files: check it out in the
    // background so the diff is usable immediately.
    let (tx, rx) = mpsc::channel();
    {
        let review = review.clone();
        std::thread::spawn(move || {
            let _ = tx.send(review.ensure_worktree().map_err(|e| e.to_string()));
        });
    }

    let mut app = App::new(review, state, state_path, cfg);
    app.start_grouping(true);
    app.start_remote();
    let mut terminal = ratatui::init();
    // Focus events tell when the Neovim float is shown again.
    let _ = crossterm::execute!(std::io::stdout(), crossterm::event::EnableFocusChange);
    let result = run(&mut terminal, &mut app, rx);
    let _ = crossterm::execute!(std::io::stdout(), crossterm::event::DisableFocusChange);
    ratatui::restore();
    result
}

fn run(
    terminal: &mut ratatui::DefaultTerminal,
    app: &mut App,
    worktree: mpsc::Receiver<Result<(), String>>,
) -> Result<()> {
    while !app.quit {
        app.poll_grouping();
        app.poll_graph();
        app.poll_lsp();
        app.poll_ask();
        app.poll_remote();
        app.poll_publish();
        if std::mem::take(&mut app.sh.needs_clear) {
            terminal.clear()?;
        }
        terminal.draw(|f| ui::render(f, app))?;
        // The diff is on screen: build the code graph in the background.
        if matches!(app.graph_status, GraphStatus::NotStarted) {
            app.start_graph();
        }

        if let Ok(res) = worktree.try_recv() {
            match res {
                Ok(()) => app.sh.worktree_ready = true,
                Err(e) => app.sh.notify(format!("worktree checkout failed: {e}")),
            }
        }
        // Graph and worktree ready: refine the graph with language servers.
        app.maybe_start_lsp();
        if event::poll(Duration::from_millis(250))? {
            match event::read()? {
                Event::Key(k) if k.kind == KeyEventKind::Press => app.on_key(k),
                // Shown again (survol.nvim float, tmux pane...): repaint
                // everything rather than trust the terminal's copy.
                Event::Resize(..) | Event::FocusGained => app.sh.needs_clear = true,
                _ => {}
            }
        }
    }
    Ok(())
}
