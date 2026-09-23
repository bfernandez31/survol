//! Supported languages: grammar and query of each.

use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use tree_sitter::{Language, Query};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Lang {
    Java,
    Kotlin,
    TypeScript,
    Tsx,
    JavaScript,
}

impl Lang {
    pub const ALL: [Lang; 5] = [
        Lang::Java,
        Lang::Kotlin,
        Lang::TypeScript,
        Lang::Tsx,
        Lang::JavaScript,
    ];

    pub fn from_path(path: &str) -> Option<Self> {
        Some(match crate::diff::language_of(path)? {
            "java" => Lang::Java,
            "kotlin" => Lang::Kotlin,
            "typescript" => Lang::TypeScript,
            "tsx" => Lang::Tsx,
            "javascript" => Lang::JavaScript,
            _ => return None,
        })
    }

    /// Short tag, used in cache file names.
    pub fn tag(self) -> &'static str {
        match self {
            Lang::Java => "java",
            Lang::Kotlin => "kt",
            Lang::TypeScript => "ts",
            Lang::Tsx => "tsx",
            Lang::JavaScript => "js",
        }
    }

    /// Java and Kotlin: packages, static types, classes as the unit of code.
    pub fn is_jvm(self) -> bool {
        matches!(self, Lang::Java | Lang::Kotlin)
    }

    pub fn grammar(self) -> Language {
        match self {
            Lang::Java => tree_sitter_java::LANGUAGE.into(),
            Lang::Kotlin => tree_sitter_kotlin_ng::LANGUAGE.into(),
            Lang::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Lang::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
            Lang::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
        }
    }

    pub(super) fn query_source(self) -> &'static str {
        match self {
            Lang::Java => include_str!("../../queries/java.scm"),
            Lang::Kotlin => include_str!("../../queries/kotlin.scm"),
            Lang::TypeScript | Lang::Tsx => include_str!("../../queries/typescript.scm"),
            Lang::JavaScript => include_str!("../../queries/javascript.scm"),
        }
    }

    /// The compiled index query (compiled once per process).
    pub fn query(self) -> &'static Query {
        static QUERIES: [OnceLock<Query>; 5] = [const { OnceLock::new() }; 5];
        QUERIES[self as usize].get_or_init(|| {
            Query::new(&self.grammar(), self.query_source())
                .unwrap_or_else(|e| panic!("invalid {self:?} query: {e}"))
        })
    }
}
