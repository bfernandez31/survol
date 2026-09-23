//! Opening `file:line`: in the parent Neovim when run from it (`$NVIM`),
//! otherwise in `$VISUAL` / `$EDITOR` in the foreground.
//!
//! In Neovim, survol.nvim's `open_file` is called over RPC: it hides the
//! survol float (the TUI keeps running) and opens the file in the user's
//! window. Without the plugin, keys doing `:tabedit` are sent instead.

use std::io;
use std::path::Path;
use std::process::{Command, Output, Stdio};

use crossterm::cursor::{Hide, Show};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};

pub enum Opened {
    /// Opened by survol.nvim, which hid the survol float.
    Plugin,
    /// Sent to the parent Neovim as keys (plugin not loaded).
    Parent,
    /// An editor ran in the foreground; the screen must be redrawn.
    Foreground,
}

pub fn open(path: &Path, line: u32) -> io::Result<Opened> {
    match std::env::var("NVIM").ok().filter(|s| !s.is_empty()) {
        Some(server) => open_in_nvim(&server, path, line),
        None => open_foreground(path, line),
    }
}

fn open_in_nvim(server: &str, path: &Path, line: u32) -> io::Result<Opened> {
    let out = nvim(server, "--remote-expr", &plugin_expr(path, line, 0))?;
    if !out.status.success() {
        return Err(nvim_error(server, &out));
    }
    match parse_reply(&String::from_utf8_lossy(&out.stdout)) {
        Reply::Ok => Ok(Opened::Plugin),
        Reply::Error(e) => Err(io::Error::other(e)),
        Reply::NoPlugin => {
            let out = nvim(server, "--remote-send", &remote_keys(path, line))?;
            if out.status.success() {
                Ok(Opened::Parent)
            } else {
                Err(nvim_error(server, &out))
            }
        }
    }
}

fn nvim(server: &str, flag: &str, arg: &str) -> io::Result<Output> {
    Command::new("nvim")
        .args(["--server", server, flag, arg])
        // Keep the client off the TUI's terminal.
        .stdin(Stdio::null())
        .output()
}

fn nvim_error(server: &str, out: &Output) -> io::Error {
    let stderr = String::from_utf8_lossy(&out.stderr);
    let msg = stderr.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    io::Error::other(format!("nvim --server {server}: {}", msg.trim()))
}

#[derive(Debug, PartialEq)]
enum Reply {
    Ok,
    NoPlugin,
    Error(String),
}

fn parse_reply(stdout: &str) -> Reply {
    match stdout.trim() {
        "ok" => Reply::Ok,
        "noplugin" => Reply::NoPlugin,
        "" => Reply::Error("no reply from Neovim".into()),
        other => Reply::Error(
            other
                .strip_prefix("error: ")
                .unwrap_or(other)
                .trim()
                .to_string(),
        ),
    }
}

/// Vim single-quoted string literal.
fn vim_str(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// Expression calling survol.nvim's `open_file(path, line, col)`, or
/// returning `noplugin` when the module is not on the runtimepath. The
/// arguments go through `_A`, so the path is never Lua-escaped.
fn plugin_expr(path: &Path, line: u32, col: u32) -> String {
    const LUA: &str = "(function(a) \
        local ok, s = pcall(require, 'survol') \
        if not ok or type(s.open_file) ~= 'function' then return 'noplugin' end \
        return s.open_file(a[1], a[2], a[3]) \
        end)(_A)";
    format!(
        "luaeval({}, [{}, {line}, {col}])",
        vim_str(LUA),
        vim_str(&path.to_string_lossy())
    )
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

/// Runs `$VISUAL` / `$EDITOR` (default `nvim`) on the TUI's terminal: the
/// TUI screen is suspended, then restored (the caller redraws everything).
fn open_foreground(path: &Path, line: u32) -> io::Result<Opened> {
    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .ok()
        .filter(|e| !e.trim().is_empty())
        .unwrap_or_else(|| "nvim".into());
    let mut parts = editor.split_whitespace();
    let program = parts.next().unwrap_or("nvim");

    suspend()?;
    let result = Command::new(program)
        .args(parts)
        .arg(format!("+{line}"))
        .arg(path)
        .status();
    // Restore even when the editor failed to start.
    resume()?;
    let status = result.map_err(|e| io::Error::other(format!("{program}: {e}")))?;
    if !status.success() {
        return Err(io::Error::other(format!("{program} exited with {status}")));
    }
    Ok(Opened::Foreground)
}

fn suspend() -> io::Result<()> {
    execute!(io::stdout(), LeaveAlternateScreen, Show)?;
    disable_raw_mode()
}

fn resume() -> io::Result<()> {
    enable_raw_mode()?;
    execute!(io::stdout(), EnterAlternateScreen, Hide)
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

    #[test]
    fn plugin_expr_passes_path_as_vim_string() {
        let e = plugin_expr(Path::new("/w/my dir/it's \"x\".ts"), 42, 3);
        assert!(e.starts_with("luaeval('(function(a) local ok, s = pcall(require, ''survol'')"));
        assert!(
            e.ends_with(r#"(_A)', ['/w/my dir/it''s "x".ts', 42, 3])"#),
            "{e}"
        );
    }

    #[test]
    fn parses_plugin_replies() {
        assert_eq!(parse_reply("ok"), Reply::Ok);
        assert_eq!(parse_reply("ok\n"), Reply::Ok);
        assert_eq!(parse_reply("noplugin"), Reply::NoPlugin);
        assert_eq!(
            parse_reply("error: not readable: /x"),
            Reply::Error("not readable: /x".into())
        );
        assert_eq!(parse_reply(""), Reply::Error("no reply from Neovim".into()));
    }
}
