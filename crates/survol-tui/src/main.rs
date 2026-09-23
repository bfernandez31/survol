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
        Opens a GitLab merge request (or a local range) in a terminal review UI. \
        Run `survol-cli doctor` to check your setup."
)]
struct Cli {
    /// Merge request number (`123`, `!123`), merge request URL, or local range
    /// `base..head`. Empty: the merge request of the current branch.
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
    let mut terminal = ratatui::init();
    let result = run(&mut terminal, &mut app, rx);
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
        if event::poll(Duration::from_millis(250))? {
            match event::read()? {
                Event::Key(k) if k.kind == KeyEventKind::Press => app.on_key(k),
                _ => {}
            }
        }
    }
    Ok(())
}
