//! Environment checks behind `survol-cli doctor`.

use std::path::Path;
use std::process::Command;

use serde::Serialize;

use crate::config::{Config, LlmConfig};
use crate::forge::Forge;
use crate::forge::gitlab::{self, Gitlab};
use crate::git::Git;
use crate::llm::claude_command;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Ok,
    /// Works, but a feature is degraded.
    Warn,
    /// Blocks the main use case.
    Fail,
}

#[derive(Debug, Clone, Serialize)]
pub struct Check {
    pub name: &'static str,
    pub status: Status,
    pub detail: String,
}

fn check(name: &'static str, status: Status, detail: impl Into<String>) -> Check {
    Check {
        name,
        status,
        detail: detail.into(),
    }
}

/// First line of `cmd args` stdout, if it runs successfully.
fn probe(cmd: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(cmd).args(args).output().ok()?;
    out.status.success().then(|| first_line(&out.stdout))
}

pub fn run(cwd: &Path, cfg: &Config) -> Vec<Check> {
    let mut checks = Vec::new();

    checks.push(match Git::version() {
        Ok(v) => check("git", Status::Ok, v),
        Err(e) => check("git", Status::Fail, e.to_string()),
    });
    checks.push(match Git::discover(cwd) {
        Some(g) => check("repository", Status::Ok, g.dir().display().to_string()),
        None => check("repository", Status::Warn, "not inside a git repository"),
    });

    checks.extend(gitlab_checks(cfg));

    checks.push(match probe("glab", &["--version"]) {
        Some(v) => check("glab", Status::Ok, v),
        None => check(
            "glab",
            Status::Warn,
            "not installed (GITLAB_TOKEN is used instead)",
        ),
    });

    checks.push(match probe("nvim", &["--version"]) {
        Some(v) => {
            let parent = std::env::var("NVIM").ok().filter(|s| !s.is_empty());
            let detail = match parent {
                Some(sock) => format!("{v}, parent server {sock}"),
                None => format!("{v}, no parent ($NVIM unset): files open in $EDITOR"),
            };
            check("nvim", Status::Ok, detail)
        }
        None => check("nvim", Status::Warn, "not installed: files open in $EDITOR"),
    });

    checks.push(llm_check(cwd, &cfg.llm));
    checks.extend(lsp_checks(&cfg.lsp));
    checks
}

/// One check per language server family: found in `PATH` or not (servers
/// are not started: jdtls alone takes seconds).
fn lsp_checks(cfg: &crate::config::LspConfig) -> Vec<Check> {
    use crate::lsp::{self, Family, Unavailable};
    lsp::detect(cfg)
        .into_iter()
        .map(|(family, res)| {
            let name = match family {
                Family::Java => "lsp java",
                Family::Kotlin => "lsp kotlin",
                Family::Typescript => "lsp ts/js",
            };
            match res {
                Ok(spec) => check(
                    name,
                    Status::Ok,
                    format!("{} ({})", spec.name(), spec.path.display()),
                ),
                Err(Unavailable::Disabled) => check(name, Status::Ok, "disabled"),
                Err(Unavailable::Missing(cmd)) => {
                    let install: Vec<&str> =
                        lsp::candidates(family).iter().map(|c| c.install).collect();
                    check(
                        name,
                        Status::Warn,
                        format!(
                            "{cmd} not found: heuristic graph only (install: {})",
                            install.join(" or ")
                        ),
                    )
                }
            }
        })
        .collect()
}

fn gitlab_checks(cfg: &Config) -> Vec<Check> {
    let Some(host) = cfg.gitlab.host.as_deref() else {
        return vec![check(
            "gitlab host",
            Status::Fail,
            "not configured: set GITLAB_HOST or [gitlab] host in ~/.config/survol/config.toml",
        )];
    };
    let mut checks = vec![check("gitlab host", Status::Ok, gitlab::base_url(host))];
    if let Some(ca) = &cfg.gitlab.ca_cert {
        checks.push(if ca.is_file() {
            check("ca certificate", Status::Ok, ca.display().to_string())
        } else {
            check(
                "ca certificate",
                Status::Fail,
                format!("{} not found", ca.display()),
            )
        });
    }
    match gitlab::token_source(host) {
        Some(src) => checks.push(check("gitlab token", Status::Ok, format!("from {src}"))),
        None => {
            checks.push(check(
                "gitlab token",
                Status::Fail,
                format!(
                    "none: `glab auth login --hostname {}` or set GITLAB_TOKEN",
                    gitlab::bare_host(host)
                ),
            ));
            return checks;
        }
    }
    checks.push(
        match Gitlab::from_config(&cfg.gitlab).and_then(|g| g.server_version()) {
            Ok(v) => check("gitlab api", Status::Ok, format!("version {v}")),
            Err(e) => check("gitlab api", Status::Fail, e.to_string()),
        },
    );
    checks
}

fn llm_check(cwd: &Path, cfg: &LlmConfig) -> Check {
    let command = cfg.command.as_str();
    let config_dir = cfg.config_dir();
    let run = |args: &[&str]| {
        claude_command(command, config_dir.as_deref(), cwd)
            .args(args)
            .output()
            .ok()
    };
    let Some(version) = run(&["--version"])
        .filter(|o| o.status.success())
        .map(|o| first_line(&o.stdout))
    else {
        return check(
            "llm",
            Status::Warn,
            format!("`{command}` not found: no grouping nor questions"),
        );
    };
    let status = run(&["auth", "status"])
        .and_then(|o| serde_json::from_slice::<serde_json::Value>(&o.stdout).ok());
    let dir = match &config_dir {
        Some(d) => format!("CLAUDE_CONFIG_DIR={}", d.display()),
        None => "default config dir".into(),
    };
    let Some(status) = status else {
        return check("llm", Status::Ok, format!("{command} {version} ({dir})"));
    };
    if status.get("loggedIn").and_then(|b| b.as_bool()) == Some(false) {
        let env = config_dir
            .as_ref()
            .map(|d| format!("CLAUDE_CONFIG_DIR={} ", d.display()))
            .unwrap_or_default();
        return check(
            "llm",
            Status::Warn,
            format!(
                "{command} {version}, not authenticated ({dir}): run `{env}{command}` and log in"
            ),
        );
    }
    check(
        "llm",
        Status::Ok,
        format!("{command} {version}, {} ({dir})", account(&status)),
    )
}

/// "authenticated as <email>, <org>, <plan>" from `claude auth status` JSON,
/// with whatever fields are present.
fn account(status: &serde_json::Value) -> String {
    let field = |k: &str| {
        status
            .get(k)
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
    };
    let mut s = "authenticated".to_string();
    if let Some(email) = field("email") {
        s.push_str(&format!(" as {email}"));
    }
    let details: Vec<&str> = ["orgName", "subscriptionType", "authMethod"]
        .iter()
        .filter_map(|k| field(k))
        .collect();
    if !details.is_empty() {
        s.push_str(&format!(" [{}]", details.join(", ")));
    }
    s
}

fn first_line(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describes_account() {
        let v = serde_json::json!({"loggedIn": true, "email": "me@corp.example",
            "orgName": "Corp", "subscriptionType": "team", "authMethod": "claude.ai"});
        assert_eq!(
            account(&v),
            "authenticated as me@corp.example [Corp, team, claude.ai]"
        );
        assert_eq!(account(&serde_json::json!({})), "authenticated");
    }
}
