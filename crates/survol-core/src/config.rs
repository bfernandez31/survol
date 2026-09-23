//! Configuration: `~/.config/survol/config.toml`, overridden by the project's
//! `.survol/config.toml`, overridden by environment variables.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("cannot read {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("invalid config {path}: {source}")]
    Parse {
        path: PathBuf,
        source: toml::de::Error,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub gitlab: GitlabConfig,
    pub git: GitConfig,
    pub review: ReviewConfig,
    pub llm: LlmConfig,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GitlabConfig {
    /// Self-hosted instance, e.g. `gitlab.corp.example` or `https://gitlab.corp.example`.
    /// Required; overridden by `GITLAB_HOST`.
    pub host: Option<String>,
    /// Extra PEM bundle trusted on top of the system store (corporate CA).
    pub ca_cert: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GitConfig {
    pub remote: String,
}

impl Default for GitConfig {
    fn default() -> Self {
        Self {
            remote: "origin".into(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ReviewConfig {
    /// Extra globs for the mechanical / noise group, added to the built-in ones.
    pub mechanical_globs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LlmConfig {
    /// `false`: never call the LLM (privacy, offline); the Stack view groups
    /// by directory.
    pub enabled: bool,
    pub command: String,
    /// Fast model used to group hunks (`None`: the CLI's default).
    pub group_model: Option<String>,
    /// Reasoning effort for grouping (`low`, `medium`, `high`...). Low keeps
    /// big groupings fast: extended thinking dominated latency and cost.
    pub group_effort: Option<String>,
    /// Stronger model used to answer questions.
    pub ask_model: Option<String>,
    /// Claude Code configuration directory (`CLAUDE_CONFIG_DIR`), to use
    /// another account than the default one. A leading `~` is expanded.
    pub config_dir: Option<PathBuf>,
    /// Maximum prompt size in characters, a proxy for the token budget.
    /// Bigger reviews are grouped by module, then merged.
    pub max_prompt_chars: usize,
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            command: "claude".into(),
            group_model: None,
            group_effort: Some("low".into()),
            ask_model: None,
            config_dir: None,
            max_prompt_chars: 150_000,
        }
    }
}

impl LlmConfig {
    /// [`Self::config_dir`] with `~` expanded.
    pub fn config_dir(&self) -> Option<PathBuf> {
        self.config_dir.as_deref().map(expand_home)
    }
}

/// Expands a leading `~` to the home directory.
pub fn expand_home(path: &Path) -> PathBuf {
    let home = || directories::BaseDirs::new().map(|d| d.home_dir().to_path_buf());
    match path.strip_prefix("~") {
        Ok(rest) => match home() {
            Some(h) => h.join(rest),
            None => path.to_path_buf(),
        },
        Err(_) => path.to_path_buf(),
    }
}

impl Config {
    pub fn user_path() -> Option<PathBuf> {
        // XDG-style on every platform: terminal users expect ~/.config.
        if let Some(x) = std::env::var_os("XDG_CONFIG_HOME") {
            return Some(PathBuf::from(x).join("survol/config.toml"));
        }
        directories::BaseDirs::new().map(|d| d.home_dir().join(".config/survol/config.toml"))
    }

    pub fn project_path(repo_root: &Path) -> PathBuf {
        repo_root.join(".survol/config.toml")
    }

    /// Loads and merges the user and project files, then the environment.
    pub fn load(repo_root: Option<&Path>) -> Result<Self, ConfigError> {
        let mut value = toml::Table::new();
        let paths = Self::user_path()
            .into_iter()
            .chain(repo_root.map(Self::project_path));
        for path in paths {
            if let Some(table) = read_table(&path)? {
                merge(&mut value, table);
            }
        }
        let mut cfg: Config =
            toml::Value::Table(value)
                .try_into()
                .map_err(|source| ConfigError::Parse {
                    path: PathBuf::from("<merged>"),
                    source,
                })?;
        cfg.apply_env(|k| std::env::var(k).ok());
        Ok(cfg)
    }

    fn apply_env(&mut self, get: impl Fn(&str) -> Option<String>) {
        if let Some(h) = get("GITLAB_HOST").filter(|h| !h.is_empty()) {
            self.gitlab.host = Some(h);
        }
        if let Some(c) = get("SURVOL_CA_CERT").filter(|c| !c.is_empty()) {
            self.gitlab.ca_cert = Some(c.into());
        }
    }
}

fn read_table(path: &Path) -> Result<Option<toml::Table>, ConfigError> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(ConfigError::Read {
                path: path.into(),
                source,
            });
        }
    };
    let table: toml::Table = toml::from_str(&text).map_err(|source| ConfigError::Parse {
        path: path.into(),
        source,
    })?;
    // Validate each file on its own so errors point at the right path.
    Config::deserialize(toml::Value::Table(table.clone())).map_err(|source| {
        ConfigError::Parse {
            path: path.into(),
            source,
        }
    })?;
    Ok(Some(table))
}

fn merge(base: &mut toml::Table, over: toml::Table) {
    for (k, v) in over {
        match (base.get_mut(&k), v) {
            (Some(toml::Value::Table(b)), toml::Value::Table(o)) => merge(b, o),
            (_, v) => {
                base.insert(k, v);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_overrides_user_and_env_overrides_both() {
        let mut base: toml::Table =
            toml::from_str("[gitlab]\nhost = \"user.example\"\nca_cert = \"/ca.pem\"\n").unwrap();
        merge(
            &mut base,
            toml::from_str("[gitlab]\nhost = \"project.example\"\n[llm]\ngroup_model = \"haiku\"")
                .unwrap(),
        );
        let mut cfg: Config = toml::Value::Table(base).try_into().unwrap();
        assert_eq!(cfg.gitlab.host.as_deref(), Some("project.example"));
        assert_eq!(cfg.gitlab.ca_cert.as_deref(), Some(Path::new("/ca.pem")));
        assert_eq!(cfg.llm.command, "claude");
        assert_eq!(cfg.git.remote, "origin");

        cfg.apply_env(|k| (k == "GITLAB_HOST").then(|| "env.example".to_string()));
        assert_eq!(cfg.gitlab.host.as_deref(), Some("env.example"));
    }

    #[test]
    fn expands_home_in_llm_config_dir() {
        let cfg: Config = toml::from_str("[llm]\nconfig_dir = \"~/.claude-pro\"\n").unwrap();
        let dir = cfg.llm.config_dir().unwrap();
        assert!(dir.is_absolute(), "{dir:?}");
        assert!(dir.ends_with(".claude-pro"));
        assert_eq!(cfg.llm.max_prompt_chars, 150_000);
        assert!(cfg.llm.enabled);
        assert_eq!(expand_home(Path::new("/abs/~x")), Path::new("/abs/~x"));
    }

    #[test]
    fn rejects_unknown_keys() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("c.toml");
        std::fs::write(&p, "[gitlab]\nhots = \"x\"\n").unwrap();
        assert!(matches!(read_table(&p), Err(ConfigError::Parse { .. })));
    }
}
