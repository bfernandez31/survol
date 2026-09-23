//! Front ↔ back HTTP linking: outgoing HTTP calls matched to the endpoints
//! of the same index (`http_calls` edges, caller → endpoint method).
//!
//! Calls: Angular `HttpClient` (`this.http.get/post/put/delete/patch(url)`),
//! `axios`, `fetch`; Java / Kotlin `RestTemplate` (`getForObject`...),
//! `WebClient` / `RestClient` (`.get().uri(url)`); and symbols already
//! tagged `http.calls` by another rule (`@FeignClient` methods). The URL is
//! rebuilt from literals, template literals and concatenations, resolving
//! fields, constants and `environment.*` values; what stays unknown becomes a
//! wildcard, and an unknown base URL leaves the prefix open. Every caller
//! gets an `http.calls` tag (`GET /api/owners/*`).
//!
//! Endpoints: symbols with an `http.route` tag (Spring rule), plus their
//! `http.context_path`. Matching aligns path segments from the end: literal
//! = literal, a wildcard or `{var}` matches a segment (`{id}` ↔ `${id}` best,
//! literal ↔ `{var}` a little less); a prefix difference (`/api`, context
//! path unknown to the front) lowers the confidence. The best-scoring
//! endpoints win (confidence split among ties, none beyond
//! [`MAX_CANDIDATES`](crate::graph::resolve::MAX_CANDIDATES)).

use super::FrameworkRule;
use super::spring::{client_kind, parts_to_path};
use super::util::{Part, add_tag, binding_at, eval, receiver_name};
use crate::graph::{EdgeKind, GraphBuilder, Role, SymIdx};
use crate::index::{FileIndex, Index, Lang, Ref, RefKind};

pub struct HttpLink;

/// Highest confidence of a statically matched call.
const MATCH: f32 = 0.9;

impl FrameworkRule for HttpLink {
    fn name(&self) -> &str {
        "http-link"
    }

    fn apply(&self, index: &Index, g: &mut GraphBuilder) {
        let mut calls: Vec<Call> = Vec::new();
        for (fi, f) in index.files.iter().enumerate() {
            if !f.lang.is_code() || g.symbol(g.file_symbol(fi)).has_role(Role::Test) {
                continue;
            }
            for r in &f.refs {
                if r.kind != RefKind::Call {
                    continue;
                }
                let Some((method, url_arg)) = http_call(f, r) else {
                    continue;
                };
                let parts = eval(g, fi, r.scope, r.line, url_arg);
                let from = g.scope_symbol(fi, r.scope);
                let text = format!("{method} {}", display(&parts));
                add_tag(g, from, "http.calls", &text);
                if let Some(url) = Url::from_parts(&parts) {
                    calls.push(Call {
                        from,
                        method: method.to_string(),
                        url,
                        line: r.line,
                    });
                }
            }
        }
        // Calls declared by other rules (`@FeignClient` methods).
        for s in 0..g.symbols().len() as SymIdx {
            let sym = g.symbol(s);
            if sym.removed || calls.iter().any(|c| c.from == s) {
                continue;
            }
            let Some(tag) = sym.tags.get("http.calls") else {
                continue;
            };
            for c in tag.split(", ") {
                let Some((method, path)) = c.split_once(' ') else {
                    continue;
                };
                if let Some(url) = Url::parse(path) {
                    calls.push(Call {
                        from: s,
                        method: method.to_string(),
                        url,
                        line: sym.line,
                    });
                }
            }
        }
        if calls.is_empty() {
            return;
        }
        let endpoints = endpoints(g);
        if endpoints.is_empty() {
            return;
        }
        for call in &calls {
            let mut best: Vec<(SymIdx, f32)> = Vec::new();
            let mut best_score = 0.0f32;
            for ep in &endpoints {
                if ep.symbol == call.from {
                    continue;
                }
                let Some(score) = ep.score(call) else {
                    continue;
                };
                if score > best_score + 1e-4 {
                    best_score = score;
                    best.clear();
                }
                if (score - best_score).abs() <= 1e-4 && !best.iter().any(|b| b.0 == ep.symbol) {
                    best.push((ep.symbol, score));
                }
            }
            if best.is_empty() || best.len() > crate::graph::resolve::MAX_CANDIDATES {
                continue;
            }
            let k = best.len() as f32;
            for (to, score) in best {
                g.add_edge(
                    call.from,
                    to,
                    EdgeKind::HttpCalls,
                    MATCH * score / k,
                    call.line,
                );
            }
        }
    }
}

/// HTTP method and URL argument of a call, when it is an HTTP client call.
fn http_call<'r>(f: &FileIndex, r: &'r Ref) -> Option<(&'static str, &'r str)> {
    let arg = r.arg.as_deref()?;
    let verb = |name: &str| -> Option<&'static str> {
        Some(match name {
            "get" => "GET",
            "post" => "POST",
            "put" => "PUT",
            "delete" => "DELETE",
            "patch" => "PATCH",
            "head" => "HEAD",
            "options" => "OPTIONS",
            _ => return None,
        })
    };
    match f.lang {
        Lang::TypeScript | Lang::Tsx | Lang::JavaScript => {
            if r.name == "fetch" && r.receiver.as_deref().is_none_or(|x| x == "window") {
                return Some(("ANY", arg));
            }
            let method = verb(&r.name)?;
            let recv = r.receiver.as_deref()?;
            if recv == "axios" {
                return Some((method, arg));
            }
            let (name, this) = receiver_name(recv)?;
            let typed = binding_at(f, r.scope, name, r.line, this)
                .is_some_and(|b| matches!(b.type_name.as_str(), "HttpClient" | "AxiosInstance"));
            let named = name.to_ascii_lowercase().contains("http")
                && f.imports.iter().any(|i| i.module == "@angular/common/http");
            (typed || named).then_some((method, arg))
        }
        Lang::Java | Lang::Kotlin => {
            let recv = r.receiver.as_deref()?;
            if r.name == "uri" {
                // `client.get().uri(url)`: the verb is earlier in the chain.
                let method = ["get", "post", "put", "delete", "patch", "head", "options"]
                    .iter()
                    .filter_map(|v| recv.rfind(&format!(".{v}()")).map(|i| (i, *v)))
                    .max()
                    .and_then(|(_, v)| verb(v))?;
                return Some((method, arg));
            }
            let method = match r.name.as_str() {
                "getForObject" | "getForEntity" => "GET",
                "postForObject" | "postForEntity" | "postForLocation" => "POST",
                "put" => "PUT",
                "delete" => "DELETE",
                "patchForObject" => "PATCH",
                "exchange" => "ANY",
                _ => return None,
            };
            let (name, this) = receiver_name(recv)?;
            let typed = binding_at(f, r.scope, name, r.line, this)
                .is_some_and(|b| client_kind(&b.type_name, &b.name).is_some());
            typed.then_some((method, arg))
        }
        _ => None,
    }
}

struct Call {
    from: SymIdx,
    method: String,
    url: Url,
    line: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Seg {
    Lit(String),
    /// `{id}`, `:id`, `*`, `${id}`: any one segment.
    Var,
}

/// A normalised URL path.
#[derive(Debug, Clone, PartialEq)]
struct Url {
    segs: Vec<Seg>,
    /// Starts with an unknown base (`${base}/owners`): any prefix may precede.
    open: bool,
}

/// Placeholder for unknown parts while normalising.
const WILD: char = '\u{0}';

impl Url {
    fn from_parts(parts: &[Part]) -> Option<Url> {
        let s: String = parts
            .iter()
            .map(|p| match p {
                Part::Lit(s) => s.clone(),
                Part::Wild => WILD.to_string(),
            })
            .collect();
        Url::parse(&s)
    }

    /// `http://host:1/api/owners/{id}?x=1` → `api`, `owners`, var.
    fn parse(s: &str) -> Option<Url> {
        let s = s.split(['?', '#']).next().unwrap_or(s);
        let mut s = s.trim();
        for scheme in ["http://", "https://", "//"] {
            if let Some(rest) = s.strip_prefix(scheme) {
                s = rest.find('/').map_or("", |i| &rest[i..]);
                break;
            }
        }
        let open = s.starts_with(WILD);
        let s = s.trim_start_matches(WILD);
        let segs: Vec<Seg> = s
            .split('/')
            .filter(|x| !x.is_empty())
            .map(|x| {
                if x.contains(WILD)
                    || x.starts_with('{')
                    || x.starts_with(':')
                    || x.starts_with('*')
                {
                    Seg::Var
                } else {
                    Seg::Lit(x.to_string())
                }
            })
            .collect();
        segs.iter()
            .any(|x| matches!(x, Seg::Lit(_)))
            .then_some(Url { segs, open })
    }
}

/// Parts as shown in `http.calls`: unknown parts as `*`, host dropped.
fn display(parts: &[Part]) -> String {
    let p = parts_to_path(parts);
    let p = p.split(['?', '#']).next().unwrap_or(&p);
    for scheme in ["http://", "https://"] {
        if let Some(rest) = p.strip_prefix(scheme) {
            return rest.find('/').map_or("/".into(), |i| rest[i..].to_string());
        }
    }
    p.to_string()
}

struct Endpoint {
    symbol: SymIdx,
    method: String,
    /// Route without, then with the context path.
    variants: Vec<(Vec<Seg>, f32)>,
}

fn endpoints(g: &GraphBuilder) -> Vec<Endpoint> {
    let mut out = Vec::new();
    for (i, sym) in g.symbols().iter().enumerate() {
        let Some(routes) = sym.tags.get("http.route") else {
            continue;
        };
        let context = sym
            .tags
            .get("http.context_path")
            .and_then(|c| Url::parse(c))
            .map(|u| u.segs);
        for route in routes.split(", ") {
            let Some((method, path)) = route.split_once(' ') else {
                continue;
            };
            let Some(url) = Url::parse(path) else {
                continue;
            };
            let mut variants = vec![(url.segs.clone(), 1.0)];
            if let Some(c) = &context {
                let mut full = c.clone();
                full.extend(url.segs.iter().cloned());
                variants.push((full, 1.0));
            }
            out.push(Endpoint {
                symbol: i as SymIdx,
                method: method.to_string(),
                variants,
            });
        }
    }
    out
}

impl Endpoint {
    /// Match score of `call` against this endpoint, `None` when incompatible.
    fn score(&self, call: &Call) -> Option<f32> {
        let method = if self.method == call.method {
            1.0
        } else if self.method == "ANY" || call.method == "ANY" {
            0.8
        } else {
            return None;
        };
        let best = self
            .variants
            .iter()
            .filter_map(|(segs, w)| align(&call.url, segs).map(|s| s * w))
            .fold(None, |acc: Option<f32>, s| {
                Some(acc.map_or(s, |a| a.max(s)))
            })?;
        Some(best * method)
    }
}

/// Aligns the call path on the endpoint path from the end.
fn align(call: &Url, ep: &[Seg]) -> Option<f32> {
    let (n, m) = (call.segs.len(), ep.len());
    let common = n.min(m);
    let mut score = 1.0f32;
    let mut literal = false;
    for k in 1..=common {
        match (&call.segs[n - k], &ep[m - k]) {
            (Seg::Lit(a), Seg::Lit(b)) if a == b => literal = true,
            (Seg::Lit(_), Seg::Lit(_)) => return None,
            (Seg::Var, Seg::Var) => {}
            // `/owners/search` called, `/owners/{id}` declared.
            (Seg::Lit(_), Seg::Var) => score *= 0.85,
            // `/owners/${x}` called, `/owners/search` declared.
            (Seg::Var, Seg::Lit(_)) => score *= 0.6,
        }
    }
    if !literal {
        return None;
    }
    let prefix = if n == m {
        1.0
    } else if n < m {
        // The endpoint has more leading segments: covered by an unknown
        // base URL, else a prefix difference (`/api`).
        if call.open { 0.85 } else { 0.6 }
    } else {
        // The call has more leading segments (context path, gateway prefix).
        0.7
    };
    Some(score * prefix)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    fn segs(s: &str) -> Vec<Seg> {
        url(s).segs
    }

    #[test]
    fn urls_are_normalised() {
        assert_eq!(
            url("http://localhost:9966/petclinic/api/owners/\u{0}?q=1"),
            Url {
                segs: vec![
                    Seg::Lit("petclinic".into()),
                    Seg::Lit("api".into()),
                    Seg::Lit("owners".into()),
                    Seg::Var
                ],
                open: false
            }
        );
        let u = url("\u{0}owners/{id}");
        assert!(u.open);
        assert_eq!(u.segs, vec![Seg::Lit("owners".into()), Seg::Var]);
        assert!(Url::parse("\u{0}/\u{0}").is_none(), "no literal segment");
    }

    #[test]
    fn alignment_scores() {
        let exact = align(&url("/api/owners/\u{0}"), &segs("/api/owners/{id}")).unwrap();
        assert!((exact - 1.0).abs() < 1e-6);
        let literal_var = align(&url("/api/owners/search"), &segs("/api/owners/{id}")).unwrap();
        let literal = align(&url("/api/owners/search"), &segs("/api/owners/search")).unwrap();
        assert!(literal > literal_var);
        let missing_api = align(&url("/owners"), &segs("/api/owners")).unwrap();
        let open_base = align(&url("\u{0}/owners"), &segs("/api/owners")).unwrap();
        assert!(open_base > missing_api);
        let extra = align(&url("/petclinic/api/owners"), &segs("/api/owners")).unwrap();
        assert!(extra < 1.0);
        assert!(align(&url("/api/vets"), &segs("/api/owners")).is_none());
        assert!(align(&url("/api/owners/\u{0}"), &segs("/api/owners")).is_none());
    }
}
