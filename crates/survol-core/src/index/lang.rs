//! Supported languages: grammar and query of each code language, and the
//! resource formats read without tree-sitter (HTML templates, Spring and
//! OpenAPI configuration).

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
    /// HTML template: custom elements only.
    Html,
    /// `application*.yml`, `bootstrap*.yml`, OpenAPI specs: flattened keys.
    Yaml,
    /// `application*.properties`, `bootstrap*.properties`: keys.
    Properties,
}

impl Lang {
    /// Code languages, parsed with tree-sitter.
    pub const ALL: [Lang; 5] = [
        Lang::Java,
        Lang::Kotlin,
        Lang::TypeScript,
        Lang::Tsx,
        Lang::JavaScript,
    ];

    /// Language of a file worth indexing. YAML and properties files only
    /// when they look like Spring configuration (`application*`,
    /// `bootstrap*`) or an OpenAPI spec (name containing `openapi`,
    /// `swagger` or `api`).
    pub fn from_path(path: &str) -> Option<Self> {
        Some(match crate::diff::language_of(path)? {
            "java" => Lang::Java,
            "kotlin" => Lang::Kotlin,
            "typescript" => Lang::TypeScript,
            "tsx" => Lang::Tsx,
            "javascript" => Lang::JavaScript,
            "html" => Lang::Html,
            "yaml" if is_config_name(path) || is_api_spec_name(path) => Lang::Yaml,
            "properties" if is_config_name(path) => Lang::Properties,
            _ => return None,
        })
    }

    /// Parsed with tree-sitter (see [`Lang::ALL`]).
    pub fn is_code(self) -> bool {
        !matches!(self, Lang::Html | Lang::Yaml | Lang::Properties)
    }

    /// Short tag, used in cache file names.
    pub fn tag(self) -> &'static str {
        match self {
            Lang::Java => "java",
            Lang::Kotlin => "kt",
            Lang::TypeScript => "ts",
            Lang::Tsx => "tsx",
            Lang::JavaScript => "js",
            Lang::Html => "html",
            Lang::Yaml => "yaml",
            Lang::Properties => "properties",
        }
    }

    /// Java and Kotlin: packages, static types, classes as the unit of code.
    pub fn is_jvm(self) -> bool {
        matches!(self, Lang::Java | Lang::Kotlin)
    }

    /// Tree-sitter grammar of a code language (`None` for resources).
    pub fn grammar(self) -> Option<Language> {
        Some(match self {
            Lang::Java => tree_sitter_java::LANGUAGE.into(),
            Lang::Kotlin => tree_sitter_kotlin_ng::LANGUAGE.into(),
            Lang::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Lang::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
            Lang::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
            Lang::Html | Lang::Yaml | Lang::Properties => return None,
        })
    }

    pub(super) fn query_source(self) -> &'static str {
        match self {
            Lang::Java => include_str!("../../queries/java.scm"),
            Lang::Kotlin => include_str!("../../queries/kotlin.scm"),
            Lang::TypeScript | Lang::Tsx => include_str!("../../queries/typescript.scm"),
            Lang::JavaScript => include_str!("../../queries/javascript.scm"),
            Lang::Html | Lang::Yaml | Lang::Properties => "",
        }
    }

    /// The compiled index query of a code language (compiled once per
    /// process); `None` for resources.
    pub fn query(self) -> Option<&'static Query> {
        static QUERIES: [OnceLock<Query>; 5] = [const { OnceLock::new() }; 5];
        let grammar = self.grammar()?;
        Some(QUERIES[self as usize].get_or_init(|| {
            Query::new(&grammar, self.query_source())
                .unwrap_or_else(|e| panic!("invalid {self:?} query: {e}"))
        }))
    }
}

/// Spring configuration file name: `application*.{yml,yaml,properties}`,
/// `bootstrap*...`.
pub fn is_config_name(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    name.starts_with("application") || name.starts_with("bootstrap")
}

/// OpenAPI / Swagger spec file name (`openapi.yml`, `petstore-api.yaml`).
pub fn is_api_spec_name(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path).to_ascii_lowercase();
    name.contains("openapi") || name.contains("swagger") || name.contains("api")
}
