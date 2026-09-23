//! LLM providers. v1 only has the Claude Code CLI (`claude -p`); the trait keeps
//! the rest of the engine independent of it.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::Deserialize;

use crate::config::LlmConfig;

/// Single-turn system prompt: answer from the prompt only, no tools.
const SYSTEM_PROMPT: &str = include_str!("../prompts/system.md");

#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    #[error("could not run `{command}`: {source}")]
    Spawn {
        command: String,
        source: std::io::Error,
    },
    #[error("`{command}` failed ({status}): {stderr}")]
    Failed {
        command: String,
        status: String,
        stderr: String,
    },
    #[error("unexpected output from `{command}`: {message}")]
    Output { command: String, message: String },
    #[error("the LLM reported an error: {0}")]
    Reported(String),
}

pub type Result<T> = std::result::Result<T, LlmError>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LlmRequest {
    pub prompt: String,
    /// `None`: the provider's default model.
    pub model: Option<String>,
    /// Reasoning effort (`low`, `medium`, `high`...); `None`: the default.
    pub effort: Option<String>,
    /// Directory the provider runs from (the review's worktree).
    pub cwd: PathBuf,
}

/// Something that turns a prompt into a text answer.
/// `Sync` so that independent requests can run in parallel.
pub trait LlmProvider: Sync {
    fn complete(&self, req: &LlmRequest) -> Result<String>;
}

/// The Claude Code CLI in print mode, as a pure single-turn completion:
/// no tools, no MCP servers, no skills, no settings files, no saved session.
#[derive(Debug, Clone)]
pub struct ClaudeCli {
    pub command: String,
    /// Exported as `CLAUDE_CONFIG_DIR` to select the account.
    pub config_dir: Option<PathBuf>,
}

impl ClaudeCli {
    pub fn from_config(cfg: &LlmConfig) -> Self {
        Self {
            command: cfg.command.clone(),
            config_dir: cfg.config_dir(),
        }
    }

    /// Base command, with the account environment applied.
    pub fn command(&self, cwd: &Path) -> Command {
        claude_command(&self.command, self.config_dir.as_deref(), cwd)
    }

    fn args<'a>(model: Option<&'a str>, effort: Option<&'a str>) -> Vec<&'a str> {
        let mut args = vec![
            "-p",
            "--output-format",
            "json",
            "--tools",
            "",
            "--strict-mcp-config",
            "--disable-slash-commands",
            "--no-session-persistence",
            "--setting-sources",
            "",
            "--system-prompt",
            SYSTEM_PROMPT.trim(),
        ];
        if let Some(m) = model {
            args.extend(["--model", m]);
        }
        if let Some(e) = effort {
            args.extend(["--effort", e]);
        }
        args
    }
}

/// `command` run from `cwd`, with `CLAUDE_CONFIG_DIR` set when `config_dir` is.
pub fn claude_command(command: &str, config_dir: Option<&Path>, cwd: &Path) -> Command {
    let mut cmd = Command::new(command);
    cmd.current_dir(cwd);
    if let Some(dir) = config_dir {
        cmd.env("CLAUDE_CONFIG_DIR", dir);
    }
    cmd
}

impl LlmProvider for ClaudeCli {
    fn complete(&self, req: &LlmRequest) -> Result<String> {
        let spawn_err = |source| LlmError::Spawn {
            command: self.command.clone(),
            source,
        };
        let mut child = self
            .command(&req.cwd)
            .args(Self::args(req.model.as_deref(), req.effort.as_deref()))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(spawn_err)?;
        // Write from a thread: a big prompt would otherwise fill the pipe while
        // the child waits for us to read its output.
        let mut stdin = child.stdin.take().expect("piped stdin");
        let prompt = req.prompt.clone();
        let writer = std::thread::spawn(move || stdin.write_all(prompt.as_bytes()));
        let out = child.wait_with_output().map_err(spawn_err)?;
        let written = writer.join().expect("stdin writer");
        debug_log(&req.prompt, &out.stdout);

        if !out.status.success() {
            // The JSON envelope may still explain the failure.
            if let Ok(text) = parse_envelope(&self.command, &out.stdout) {
                return Err(LlmError::Reported(text));
            }
            return Err(LlmError::Failed {
                command: self.command.clone(),
                status: out.status.to_string(),
                stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
            });
        }
        written.map_err(spawn_err)?;
        parse_envelope(&self.command, &out.stdout)
    }
}

/// With `SURVOL_LLM_LOG=<dir>`, keeps each prompt and raw answer there
/// (`<millis>-prompt.md`, `<millis>-answer.json`) for debugging.
fn debug_log(prompt: &str, answer: &[u8]) {
    let Some(dir) = std::env::var_os("SURVOL_LLM_LOG").filter(|d| !d.is_empty()) else {
        return;
    };
    let dir = PathBuf::from(dir);
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::write(dir.join(format!("{stamp}-prompt.md")), prompt);
    let _ = std::fs::write(dir.join(format!("{stamp}-answer.json")), answer);
}

#[derive(Deserialize)]
struct Envelope {
    #[serde(default)]
    is_error: bool,
    #[serde(default)]
    result: Option<String>,
    #[serde(default)]
    subtype: Option<String>,
}

/// Extracts `result` from `claude -p --output-format json`, turning
/// `is_error` into an error.
fn parse_envelope(command: &str, stdout: &[u8]) -> Result<String> {
    let env: Envelope = serde_json::from_slice(stdout).map_err(|e| LlmError::Output {
        command: command.to_string(),
        message: e.to_string(),
    })?;
    match (env.is_error, env.result) {
        (false, Some(r)) => Ok(r),
        (true, r) => Err(LlmError::Reported(
            r.or(env.subtype).unwrap_or_else(|| "unknown error".into()),
        )),
        (false, None) => Err(LlmError::Output {
            command: command.to_string(),
            message: format!(
                "no `result` field (subtype {})",
                env.subtype.as_deref().unwrap_or("?")
            ),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_envelope() {
        let ok = br#"{"type":"result","is_error":false,"result":"{\"a\":1}"}"#;
        assert_eq!(parse_envelope("claude", ok).unwrap(), r#"{"a":1}"#);
        let err = br#"{"is_error":true,"result":"Not logged in"}"#;
        assert!(matches!(
            parse_envelope("claude", err),
            Err(LlmError::Reported(m)) if m == "Not logged in"
        ));
        let turns = br#"{"is_error":false,"subtype":"error_max_turns"}"#;
        assert!(matches!(
            parse_envelope("claude", turns),
            Err(LlmError::Output { .. })
        ));
        assert!(parse_envelope("claude", b"oops").is_err());
    }

    #[test]
    fn runs_command_with_prompt_on_stdin_and_account_env() {
        // A fake `claude` echoing its stdin and CLAUDE_CONFIG_DIR as the result.
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("fake-claude");
        std::fs::write(
            &script,
            "#!/bin/sh\nin=$(cat)\nprintf '{\"is_error\":false,\"result\":\"%s|%s|%s\"}' \"$in\" \"$CLAUDE_CONFIG_DIR\" \"$*\"\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let cli = ClaudeCli {
            command: script.to_string_lossy().into_owned(),
            config_dir: Some("/tmp/pro-account".into()),
        };
        let out = cli
            .complete(&LlmRequest {
                prompt: "hello".into(),
                model: Some("haiku".into()),
                effort: Some("low".into()),
                cwd: dir.path().into(),
            })
            .unwrap();
        let parts: Vec<_> = out.splitn(3, '|').collect();
        assert_eq!(parts[0], "hello");
        assert_eq!(parts[1], "/tmp/pro-account");
        assert!(parts[2].starts_with("-p --output-format json --tools"));
        assert!(parts[2].ends_with("--model haiku --effort low"));
    }
}
