//! Resource files read without tree-sitter: flattened keys of YAML and
//! properties files, custom elements of HTML templates. Deliberately simple
//! line scanners: enough for Spring configuration, OpenAPI paths and Angular
//! templates, not general parsers.

use super::{ConfigEntry, Element, Lang, MAX_ARG};

/// Keys (YAML, properties) or elements (HTML) of a resource file.
pub(super) fn extract(lang: Lang, src: &str) -> (Vec<ConfigEntry>, Vec<Element>) {
    match lang {
        Lang::Yaml => (yaml_keys(src), Vec::new()),
        Lang::Properties => (properties_keys(src), Vec::new()),
        Lang::Html => (Vec::new(), html_elements(src)),
        _ => (Vec::new(), Vec::new()),
    }
}

fn entry(key: String, value: &str, line: usize) -> ConfigEntry {
    let v = value.trim();
    let v = super::unquote(v).unwrap_or(v);
    ConfigEntry {
        key,
        value: (!v.is_empty()).then(|| truncate(v, MAX_ARG)),
        line: line as u32 + 1,
    }
}

fn truncate(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((i, _)) => s[..i].to_string(),
        None => s.to_string(),
    }
}

/// Leaf keys of a YAML file, dotted (`spring.datasource.url`). List items
/// keep the key of their list (`servers.url`); block scalars (`|`, `>`) are
/// skipped; every document of a multi-document file is read.
pub fn yaml_keys(src: &str) -> Vec<ConfigEntry> {
    let mut out = Vec::new();
    // (indent, key) of the open mappings.
    let mut stack: Vec<(usize, String)> = Vec::new();
    // Indent under which lines belong to a block scalar.
    let mut block: Option<usize> = None;
    for (n, raw) in src.lines().enumerate() {
        let trimmed = raw.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let mut indent = raw.len() - trimmed.len();
        if let Some(b) = block {
            if indent > b {
                continue;
            }
            block = None;
        }
        if trimmed.starts_with("---") || trimmed.starts_with("...") {
            stack.clear();
            continue;
        }
        let mut line = trimmed;
        // `- key: v`: the item's keys are one level deeper than the dash.
        while let Some(rest) = line
            .strip_prefix("- ")
            .or(line.strip_prefix('-').filter(|r| r.is_empty()))
        {
            indent += line.len() - rest.len() + (rest.len() - rest.trim_start().len());
            line = rest.trim_start();
        }
        let Some((key, value)) = split_yaml_key(line) else {
            continue;
        };
        while stack.last().is_some_and(|(i, _)| *i >= indent) {
            stack.pop();
        }
        let value = strip_yaml_comment(value).trim();
        let full = stack
            .iter()
            .map(|(_, k)| k.as_str())
            .chain(std::iter::once(key.as_str()))
            .collect::<Vec<_>>()
            .join(".");
        if value.is_empty() || value.starts_with('&') {
            stack.push((indent, key));
        } else if value.starts_with('|') || value.starts_with('>') {
            out.push(entry(full, "", n));
            block = Some(indent);
        } else {
            out.push(entry(full, value, n));
        }
    }
    out
}

/// `key: value` → (key, value); `None` for a list scalar or a non-key line.
fn split_yaml_key(line: &str) -> Option<(String, &str)> {
    let (key, value) = if let Some(q) = line.chars().next().filter(|c| *c == '"' || *c == '\'') {
        let end = line[1..].find(q)? + 1;
        let rest = line[end + 1..].trim_start().strip_prefix(':')?;
        (line[1..end].to_string(), rest)
    } else {
        // The key ends at the first `: ` (or a final `:`).
        let i = line
            .match_indices(':')
            .map(|(i, _)| i)
            .find(|&i| line[i + 1..].is_empty() || line[i + 1..].starts_with([' ', '\t']))?;
        (line[..i].trim().to_string(), &line[i + 1..])
    };
    (!key.is_empty() && !key.starts_with(['{', '[', '#'])).then_some((key, value))
}

fn strip_yaml_comment(v: &str) -> &str {
    let t = v.trim_start();
    if t.starts_with(['"', '\'']) {
        return v;
    }
    match v.find(" #") {
        Some(i) => &v[..i],
        None => v,
    }
}

/// Keys of a `.properties` file (`key=value`, `key: value`, `key value`).
pub fn properties_keys(src: &str) -> Vec<ConfigEntry> {
    let mut out = Vec::new();
    let mut continued = false;
    for (n, raw) in src.lines().enumerate() {
        let line = raw.trim();
        let was_continued = continued;
        continued = line.ends_with('\\');
        if was_continued || line.is_empty() || line.starts_with(['#', '!']) {
            continue;
        }
        let i = line.find(['=', ':', ' ', '\t']).unwrap_or(line.len());
        let key = line[..i].trim();
        let value = line[i..].trim_start_matches([' ', '\t']);
        let value = value.strip_prefix(['=', ':']).unwrap_or(value);
        if !key.is_empty() {
            out.push(entry(key.to_string(), value.trim_end_matches('\\'), n));
        }
    }
    out
}

/// Custom elements (names with a `-`) opened in an HTML template, once per
/// name and line.
pub fn html_elements(src: &str) -> Vec<Element> {
    let mut out: Vec<Element> = Vec::new();
    for (n, line) in src.lines().enumerate() {
        let bytes = line.as_bytes();
        let mut i = 0;
        while let Some(p) = line[i..].find('<') {
            let start = i + p + 1;
            let mut end = start;
            while end < bytes.len()
                && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'-' || bytes[end] == b'_')
            {
                end += 1;
            }
            let name = &line[start..end];
            if name.contains('-')
                && name.starts_with(|c: char| c.is_ascii_alphabetic())
                && !out
                    .iter()
                    .rev()
                    .take_while(|e| e.line == n as u32 + 1)
                    .any(|e| e.name == name)
            {
                out.push(Element {
                    name: name.to_ascii_lowercase(),
                    line: n as u32 + 1,
                });
            }
            i = end.max(start);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(entries: &[ConfigEntry]) -> Vec<(String, Option<String>, u32)> {
        entries
            .iter()
            .map(|e| (e.key.clone(), e.value.clone(), e.line))
            .collect()
    }

    #[test]
    fn yaml_flattens_nested_keys_lists_and_documents() {
        let src = "\
# comment
spring:
  datasource:
    url: jdbc:h2:mem:x   # inline comment
    username: \"sa\"
  jpa.open-in-view: false
servers:
  - url: http://localhost:9966/petclinic/api
paths:
  /owners/{ownerId}:
    get:
      operationId: getOwner
      description: |
        text: not a key
      tags:
        - owners
---
app:
  feature: on
";
        assert_eq!(
            keys(&yaml_keys(src)),
            vec![
                (
                    "spring.datasource.url".into(),
                    Some("jdbc:h2:mem:x".into()),
                    4
                ),
                ("spring.datasource.username".into(), Some("sa".into()), 5),
                ("spring.jpa.open-in-view".into(), Some("false".into()), 6),
                (
                    "servers.url".into(),
                    Some("http://localhost:9966/petclinic/api".into()),
                    8
                ),
                (
                    "paths./owners/{ownerId}.get.operationId".into(),
                    Some("getOwner".into()),
                    12
                ),
                ("paths./owners/{ownerId}.get.description".into(), None, 13),
                ("app.feature".into(), Some("on".into()), 19),
            ]
        );
    }

    #[test]
    fn properties_keys_and_separators() {
        let src = "# c\n! c\nserver.port=9966\nserver.servlet.context-path: /petclinic/\na.b value\nlong=a\\\n  b\nlast=\n";
        assert_eq!(
            keys(&properties_keys(src)),
            vec![
                ("server.port".into(), Some("9966".into()), 3),
                (
                    "server.servlet.context-path".into(),
                    Some("/petclinic/".into()),
                    4
                ),
                ("a.b".into(), Some("value".into()), 5),
                ("long".into(), Some("a".into()), 6),
                ("last".into(), None, 8),
            ]
        );
    }

    #[test]
    fn html_custom_elements() {
        let src = "<div>\n  <app-owner-list [owners]=\"o\"></app-owner-list><mat-icon>x</mat-icon>\n<br/><app-owner-list></app-owner-list>\n";
        let els: Vec<(String, u32)> = html_elements(src)
            .into_iter()
            .map(|e| (e.name, e.line))
            .collect();
        assert_eq!(
            els,
            vec![
                ("app-owner-list".into(), 2),
                ("mat-icon".into(), 2),
                ("app-owner-list".into(), 3),
            ]
        );
    }
}
