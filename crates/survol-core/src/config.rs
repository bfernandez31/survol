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
    pub forge: ForgeConfig,
    pub gitlab: GitlabConfig,
    pub github: GithubConfig,
    pub git: GitConfig,
    pub review: ReviewConfig,
    pub llm: LlmConfig,
    pub lsp: LspConfig,
    pub theme: ThemeConfig,
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

/// Which forge to talk to. By default it is detected from the git remote:
/// github.com or a `[github] hosts` entry is GitHub, anything else GitLab.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ForgeConfig {
    /// `github` or `gitlab`: skips the detection.
    pub kind: Option<crate::forge::ForgeKind>,
    /// Host of the forge when the remote does not say it (an SSH alias...).
    /// For GitLab, `[gitlab] host` wins.
    pub host: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GithubConfig {
    /// GitHub Enterprise Server hosts, e.g. `github.corp.example`: a remote
    /// on one of them is reviewed as GitHub. github.com needs nothing.
    pub hosts: Vec<String>,
    /// Token, when neither `GITHUB_TOKEN` / `GH_TOKEN` provide one; before
    /// `gh auth token`. Printed redacted.
    #[serde(skip_serializing_if = "Option::is_none", serialize_with = "redacted")]
    pub token: Option<String>,
    /// Extra PEM bundle trusted on top of the system store (Enterprise
    /// Server behind a corporate CA).
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
    /// Language of the LLM-written text (group titles and summaries,
    /// answers): a name or a code such as `fr`. See [`language_name`].
    pub language: String,
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
            language: "English".into(),
        }
    }
}

impl LlmConfig {
    /// [`Self::config_dir`] with `~` expanded.
    pub fn config_dir(&self) -> Option<PathBuf> {
        self.config_dir.as_deref().map(expand_home)
    }

    /// [`Self::language`] as the full English name used in prompts.
    pub fn language(&self) -> String {
        language_name(&self.language)
    }

    /// Applies a `--lang` flag: it overrides the configured language.
    pub fn override_language(&mut self, flag: Option<&str>) {
        if let Some(lang) = flag.map(str::trim).filter(|l| !l.is_empty()) {
            self.language = lang.to_string();
        }
    }
}

/// `[theme]`: colours of the TUI's diff. `syntax` picks the code colours
/// (`catppuccin-mocha`, the default, or `ansi` for the terminal palette);
/// every other key overrides a colour role with `#rrggbb`, e.g.
/// `added_bg = "#302145"`. The TUI validates the role names.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ThemeConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub syntax: Option<String>,
    #[serde(flatten)]
    pub colors: std::collections::BTreeMap<String, String>,
}

/// `[lsp]`: language servers that refine the code graph after the
/// tree-sitter pass (see [`crate::lsp`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LspConfig {
    /// `false`: keep the heuristic graph, never start a server.
    pub enabled: bool,
    /// Hard limit for a whole refinement (server start and indexing
    /// included), in seconds.
    pub budget_secs: u64,
    /// Limit of one request, in seconds.
    pub request_timeout_secs: u64,
    pub java: LspServerConfig,
    pub kotlin: LspServerConfig,
    /// TypeScript and JavaScript.
    pub typescript: LspServerConfig,
}

impl Default for LspConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            budget_secs: 60,
            request_timeout_secs: 10,
            java: LspServerConfig::default(),
            kotlin: LspServerConfig::default(),
            typescript: LspServerConfig::default(),
        }
    }
}

/// One language server. Without `command`, the built-in candidates are
/// tried in order (see [`crate::lsp::candidates`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LspServerConfig {
    pub enabled: bool,
    /// Executable, looked up in `PATH` (a leading `~` is expanded).
    pub command: Option<String>,
    /// Arguments; `{data}` is replaced by a per-workspace directory under
    /// `.git/survol/lsp/` (jdtls `-data`, kotlin-lsp `--system-path`).
    pub args: Vec<String>,
    /// Extra environment variables, e.g. `JAVA_HOME` for a server that
    /// needs another JDK than the default one.
    pub env: std::collections::BTreeMap<String, String>,
}

impl Default for LspServerConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            command: None,
            args: Vec::new(),
            env: Default::default(),
        }
    }
}

/// Full English name of a language given by name or code, for prompts:
/// common codes and native names (`fr`, `français`, `DE`...) are mapped,
/// anything else is passed through as given. Empty means English.
pub fn language_name(input: &str) -> String {
    let lang = input.trim();
    let name = match lang.to_lowercase().as_str() {
        "" | "en" | "english" => "English",
        "fr" | "french" | "français" | "francais" => "French",
        "de" | "german" | "deutsch" => "German",
        "es" | "spanish" | "español" | "espanol" => "Spanish",
        "it" | "italian" | "italiano" => "Italian",
        "pt" | "portuguese" | "português" | "portugues" => "Portuguese",
        "nl" | "dutch" | "nederlands" => "Dutch",
        _ => lang,
    };
    name.to_string()
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

/// Secrets are never printed (`survol-cli config`).
fn redacted<S: serde::Serializer>(v: &Option<String>, s: S) -> Result<S::Ok, S::Error> {
    match v {
        Some(_) => s.serialize_str("<redacted>"),
        None => s.serialize_none(),
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
    fn maps_language_codes() {
        assert_eq!(language_name("fr"), "French");
        assert_eq!(language_name(" FR "), "French");
        assert_eq!(language_name("français"), "French");
        assert_eq!(language_name("en"), "English");
        assert_eq!(language_name("English"), "English");
        assert_eq!(language_name(""), "English");
        assert_eq!(language_name("de"), "German");
        assert_eq!(language_name("es"), "Spanish");
        assert_eq!(language_name("it"), "Italian");
        assert_eq!(language_name("pt"), "Portuguese");
        assert_eq!(language_name("nl"), "Dutch");
        assert_eq!(
            language_name("Brazilian Portuguese"),
            "Brazilian Portuguese"
        );
        assert_eq!(language_name("ja"), "ja");
    }

    #[test]
    fn lang_flag_overrides_config_language() {
        assert_eq!(Config::default().llm.language(), "English");
        let mut cfg: Config = toml::from_str("[llm]\nlanguage = \"fr\"\n").unwrap();
        assert_eq!(cfg.llm.language(), "French");
        cfg.llm.override_language(None);
        assert_eq!(cfg.llm.language(), "French");
        cfg.llm.override_language(Some("  "));
        assert_eq!(cfg.llm.language(), "French");
        cfg.llm.override_language(Some("de"));
        assert_eq!(cfg.llm.language(), "German");
    }

    #[test]
    fn rejects_unknown_keys() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("c.toml");
        std::fs::write(&p, "[gitlab]\nhots = \"x\"\n").unwrap();
        assert!(matches!(read_table(&p), Err(ConfigError::Parse { .. })));
    }

    #[test]
    fn reads_lsp_servers() {
        let cfg: Config = toml::from_str(
            "[lsp]\nbudget_secs = 30\n[lsp.java]\ncommand = \"jdtls\"\nargs = [\"-data\", \"{data}\"]\n[lsp.kotlin]\nenabled = false\nenv = { JAVA_HOME = \"/jdk21\" }\n",
        )
        .unwrap();
        assert!(cfg.lsp.enabled);
        assert_eq!(cfg.lsp.budget_secs, 30);
        assert_eq!(cfg.lsp.request_timeout_secs, 10);
        assert_eq!(cfg.lsp.java.command.as_deref(), Some("jdtls"));
        assert_eq!(cfg.lsp.java.args, ["-data", "{data}"]);
        assert!(!cfg.lsp.kotlin.enabled);
        assert_eq!(cfg.lsp.kotlin.env["JAVA_HOME"], "/jdk21");
        assert!(cfg.lsp.typescript.enabled && cfg.lsp.typescript.command.is_none());
    }
}
