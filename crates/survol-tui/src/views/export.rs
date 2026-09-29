//! Mermaid exports of the Graph view (module map, flows): a Markdown file
//! in `.git/survol/exports/` (`x`), with a self-contained HTML page next to
//! it opened in the browser (`X`), and the diagram offered to the overall
//! comment of the review (`S`), where GitLab renders it.

use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::app::Shared;

/// A diagram just exported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagram {
    pub title: String,
    pub mermaid: String,
}

impl Diagram {
    /// The diagram as a Markdown section, for the overall comment.
    pub fn markdown(&self) -> String {
        format!("#### {}\n\n```mermaid\n{}```\n", self.title, self.mermaid)
    }
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// A page drawing the diagram: only the mermaid script comes from the
/// network, the diagram stays in the file.
pub fn html(d: &Diagram) -> String {
    format!(
        r#"<!doctype html>
<meta charset="utf-8">
<title>{title}</title>
<body style="background:#1a1b26;color:#c0caf5;font-family:sans-serif;margin:2em">
<h2>{title}</h2>
<pre class="mermaid">
{code}</pre>
<script type="module">
  import mermaid from "https://cdn.jsdelivr.net/npm/mermaid@11/dist/mermaid.esm.min.mjs";
  mermaid.initialize({{ startOnLoad: true, theme: "dark", maxTextSize: 500000 }});
</script>
</body>
"#,
        title = escape(&d.title),
        code = escape(&d.mermaid)
    )
}

/// Writes `<stem>.md` (`markdown`) in `dir`, and `<stem>.html` with `html`.
pub fn write(
    dir: &Path,
    stem: &str,
    markdown: &str,
    d: &Diagram,
    with_html: bool,
) -> io::Result<(PathBuf, Option<PathBuf>)> {
    std::fs::create_dir_all(dir)?;
    let md = dir.join(format!("{stem}.md"));
    std::fs::write(&md, markdown)?;
    let page = if with_html {
        let p = dir.join(format!("{stem}.html"));
        std::fs::write(&p, html(d))?;
        Some(p)
    } else {
        None
    };
    Ok((md, page))
}

/// Opens `path` with the desktop's default application, without waiting.
pub fn open_in_browser(path: &Path) -> io::Result<()> {
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    Command::new(opener)
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
}

/// Writes the export `stem` of `d` and tells where; `open`: also the HTML
/// page, opened in the browser. Returns whether it was written.
pub fn export(sh: &mut Shared, stem: &str, markdown: &str, d: &Diagram, open: bool) -> bool {
    let dir = match sh.review.repo.survol_dir() {
        Ok(d) => d.join("exports"),
        Err(e) => {
            sh.notify(format!("cannot export: {e}"));
            return false;
        }
    };
    let (md, page) = match write(&dir, stem, markdown, d, open) {
        Ok(p) => p,
        Err(e) => {
            sh.notify(format!("cannot write in {}: {e}", dir.display()));
            return false;
        }
    };
    let add = "S adds it to the overall comment";
    match page {
        None => sh.notify(format!("written to {} · X opens it · {add}", md.display())),
        Some(p) => match open_in_browser(&p) {
            Ok(()) => sh.notify(format!("written and opened: {} · {add}", p.display())),
            Err(e) => sh.notify(format!(
                "written to {}, cannot open it ({e}) · {add}",
                p.display()
            )),
        },
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn diagram() -> Diagram {
        Diagram {
            title: "Module map of !7 <Orders>".into(),
            mermaid: "flowchart LR\n  a[\"x<br/>2\"] -->|1| b\n".into(),
        }
    }

    #[test]
    fn html_page_escapes_the_diagram() {
        let page = html(&diagram());
        assert!(page.contains("<title>Module map of !7 &lt;Orders&gt;</title>"));
        assert!(page.contains("a[&quot;x&lt;br/&gt;2&quot;] --&gt;|1| b"));
        assert!(page.contains("cdn.jsdelivr.net/npm/mermaid@11"));
        assert!(!page.contains("<br/>"), "nothing of the diagram is markup");
    }

    #[test]
    fn writes_markdown_and_html_side_by_side() {
        let dir = tempfile::tempdir().unwrap();
        let d = diagram();
        let (md, page) = write(dir.path(), "modules-abc", "# map\n", &d, true).unwrap();
        assert_eq!(std::fs::read_to_string(md).unwrap(), "# map\n");
        let page = page.unwrap();
        assert_eq!(page, dir.path().join("modules-abc.html"));
        assert!(
            std::fs::read_to_string(page)
                .unwrap()
                .contains("class=\"mermaid\"")
        );
        let (_, none) = write(dir.path(), "x", "", &d, false).unwrap();
        assert!(none.is_none());
        assert_eq!(
            d.markdown(),
            "#### Module map of !7 <Orders>\n\n```mermaid\nflowchart LR\n  a[\"x<br/>2\"] -->|1| b\n```\n"
        );
    }
}
