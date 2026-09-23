//! survol engine: forge access, git, diff parsing, review state, LLM grouping.
//! Everything here is UI-agnostic; the TUI and the JSON CLI build on it.

pub mod config;
pub mod diff;
pub mod doctor;
pub mod forge;
pub mod git;
pub mod group;
pub mod llm;
pub mod mechanical;
pub mod model;
pub mod review;
pub mod review_state;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Config(#[from] config::ConfigError),
    #[error(transparent)]
    Git(#[from] git::GitError),
    #[error(transparent)]
    Forge(#[from] forge::ForgeError),
    #[error(transparent)]
    Diff(#[from] diff::ParseError),
    #[error("invalid mechanical glob: {0}")]
    Glob(#[from] globset::Error),
    #[error(transparent)]
    Llm(#[from] llm::LlmError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Target(String),
}

pub type Result<T> = std::result::Result<T, Error>;
