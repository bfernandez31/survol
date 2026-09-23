//! Opening `file:line`: in the parent Neovim when run from it (`$NVIM`),
//! otherwise in `$VISUAL` / `$EDITOR` in the foreground.

use std::io;
use std::path::Path;
use std::process::Command;

use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};

pub enum Opened {
    /// Sent to the parent Neovim; the TUI keeps running.
    Parent,
    /// An editor ran in the foreground; the screen must be redrawn.
    Foreground,
}

pub fn open(path: &Path, line: u32) -> io::Result<Opened> {
    if let Some(server) = std::env::var("NVIM").ok().filter(|s| !s.is_empty()) {
        let status = Command::new("nvim")
            .args([
                "--server",
                &server,
                "--remote-send",
                &remote_keys(path, line),
            ])
            .status()?;
        return if status.success() {
            Ok(Opened::Parent)
        } else {
            Err(io::Error::other(format!("nvim --server {server} failed")))
        };
    }

    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .ok()
        .filter(|e| !e.trim().is_empty())
        .unwrap_or_else(|| "nvim".into());
    let mut parts = editor.split_whitespace();
    let program = parts.next().unwrap_or("nvim");

    disable_raw_mode()?;
    execute!(io::stdout(), LeaveAlternateScreen)?;
    let result = Command::new(program)
        .args(parts)
        .arg(format!("+{line}"))
        .arg(path)
        .status();
    execute!(io::stdout(), EnterAlternateScreen)?;
    enable_raw_mode()?;
    result?;
    Ok(Opened::Foreground)
}

/// Keys making Neovim open `path` at `line` in a new tab, from any mode.
fn remote_keys(path: &Path, line: u32) -> String {
    // Vim single-quoted string, then `<` escaped for key notation.
    let quoted = path
        .to_string_lossy()
        .replace('\'', "''")
        .replace('<', "<lt>");
    format!("<C-\\><C-N>:execute 'tabedit +{line} ' .. fnameescape('{quoted}')<CR>")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_remote_keys() {
        assert_eq!(
            remote_keys(Path::new("/w/it's <a>.ts"), 12),
            "<C-\\><C-N>:execute 'tabedit +12 ' .. fnameescape('/w/it''s <lt>a>.ts')<CR>"
        );
    }
}
