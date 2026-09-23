//! Environment checks behind `survol-cli doctor`.

use std::path::Path;
use std::process::Command;

use serde::Serialize;

use crate::config::Config;
use crate::forge::Forge;
use crate::forge::gitlab::{self, Gitlab};
use crate::git::Git;

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
    out.status.success().then(|| {
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .next()
            .unwrap_or("")
            .trim()
            .to_string()
    })
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

    checks.push(llm_check(&cfg.llm.command));
    checks
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

fn llm_check(command: &str) -> Check {
    let Some(version) = probe(command, &["--version"]) else {
        return check(
            "llm",
            Status::Warn,
            format!("`{command}` not found: no grouping nor questions"),
        );
    };
    let logged_in = Command::new(command)
        .args(["auth", "status"])
        .output()
        .ok()
        .and_then(|o| serde_json::from_slice::<serde_json::Value>(&o.stdout).ok())
        .and_then(|v| v.get("loggedIn").and_then(|b| b.as_bool()));
    match logged_in {
        Some(true) => check(
            "llm",
            Status::Ok,
            format!("{command} {version}, authenticated"),
        ),
        Some(false) => check(
            "llm",
            Status::Warn,
            format!("{command} {version}, not authenticated: run `{command}` and log in"),
        ),
        None => check("llm", Status::Ok, format!("{command} {version}")),
    }
}
