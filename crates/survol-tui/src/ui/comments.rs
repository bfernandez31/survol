//! Rendering of the comment editor and of the Review panel.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};
use survol_core::comments::{self, Anchor, Mode, Placement};
use survol_core::forge::gitlab::encode;

use survol_core::model::LineKind;

use super::ask::{centered, popup_block};
use super::rows::truncate_right;
use super::{ACCENT, ADDED_BG, CURSOR_BG, REMOVED_BG, markdown, wrap};
use crate::app::{App, RemoteStatus};
use crate::views::comments::{Confirm, Editor, NoteKind, PanelRow, ThreadView, Tone, panel_rows};

pub fn render_editor(f: &mut Frame, area: Rect, e: &Editor) {
    let w = 100.min(area.width);
    let width = w.saturating_sub(4) as usize;
    let mut lines: Vec<Line> = e
        .context
        .iter()
        .map(|c| Line::from(format!(" │ {}", truncate_right(c, width.saturating_sub(3)))).dim())
        .collect();
    if !lines.is_empty() {
        lines.push(Line::default());
    }
    let text = format!("{}▏", e.text);
    for para in text.split('\n') {
        for l in wrap(para, width.saturating_sub(1)) {
            lines.push(Line::from(format!(" {l}")));
        }
    }
    let h = (lines.len() as u16 + 4).clamp(8, area.height);
    let rect = centered(area, w, h);
    // Keep the end of a long text in view.
    let inner_h = h.saturating_sub(2) as usize;
    let scroll = lines.len().saturating_sub(inner_h) as u16;
    let hint = if e.confirm_discard {
        Line::from(
            " Esc again discards the text · Ctrl-s saves "
                .fg(Color::Yellow)
                .bold(),
        )
    } else {
        Line::from(" Ctrl-s (or Alt-Enter) save · Enter new line · Esc cancel ".dim())
    };
    f.render_widget(Clear, rect);
    f.render_widget(
        Paragraph::new(lines)
            .scroll((scroll, 0))
            .block(popup_block(format!(" {} ", e.title)).title_bottom(hint)),
        rect,
    );
}

fn first_line(s: &str) -> String {
    s.lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim()
        .to_string()
}

/// Where the review goes, for the panel's header.
fn target_line(app: &App) -> Line<'static> {
    let sh = &app.sh;
    match (&sh.review.mr, &app.remote_status) {
        (None, _) => Line::from(
            " local range: the drafts stay local (publishing needs a merge request)"
                .fg(Color::Yellow),
        ),
        (Some(_), RemoteStatus::Fetching(since)) => {
            Line::from(format!(" ⟳ asking GitLab… {}s", since.elapsed().as_secs()).fg(ACCENT))
        }
        (Some(_), RemoteStatus::Failed(e)) => {
            Line::from(format!(" GitLab unreachable: {e} (r to retry)").fg(Color::Red))
        }
        (Some(_), _) => match &sh.remote {
            Some(r) if r.capabilities.draft_notes => Line::from(
                format!(
                    " GitLab {}: drafts become draft notes, published at once",
                    r.capabilities.version
                )
                .dim(),
            ),
            Some(r) => Line::from(
                format!(
                    " GitLab {} has no draft notes: comments are posted one by one",
                    r.capabilities.version
                )
                .fg(Color::Yellow),
            ),
            None => Line::from(" GitLab: not fetched (r)".dim()),
        },
    }
}

pub fn render_panel(f: &mut Frame, area: Rect, app: &mut App) {
    let rect = centered(
        area,
        (area.width * 9 / 10).clamp(40, 130),
        (area.height * 9 / 10).max(10),
    );
    if let Some(Confirm::Publish { .. }) = &app.panel.confirm {
        return render_publish(f, rect, app);
    }
    let sh = &app.sh;
    let d = &sh.review.diff;
    let width = rect.width.saturating_sub(2) as usize;
    let discussions = sh.discussions();
    let rows = panel_rows(d, &sh.comments, discussions);
    let mut lines: Vec<Line> = vec![target_line(app), Line::default()];
    let mut sel_line = 0;
    let mut section = "";
    for (i, row) in rows.iter().enumerate() {
        let (name, count) = match row {
            PanelRow::Summary => ("", 0),
            PanelRow::Draft(_) => ("drafts", sh.comments.drafts.len()),
            PanelRow::Discussion(_) => (
                "discussions",
                discussions.iter().filter(|x| !x.is_system()).count(),
            ),
        };
        if name != section {
            section = name;
            lines.push(Line::default());
            let title = if name == "drafts" {
                format!(" Drafts ({count})")
            } else {
                format!(" Discussions on GitLab ({count})")
            };
            lines.push(Line::from(title.bold()));
        }
        if i == app.panel.sel {
            sel_line = lines.len();
        }
        let mut spans: Vec<Span> = match *row {
            PanelRow::Summary => {
                let s = &sh.comments.summary;
                vec![
                    "  Summary  ".bold().fg(ACCENT),
                    if s.trim().is_empty() {
                        "(none: Enter or S to write the overall comment)".dim()
                    } else {
                        first_line(s).into()
                    },
                ]
            }
            PanelRow::Draft(id) => {
                let Some(draft) = sh.comments.get(id) else {
                    continue;
                };
                let p = comments::place(&draft.anchor, d);
                let at = match (&draft.anchor, p) {
                    (Anchor::Reply { author, .. }, _) => format!("reply to @{author}"),
                    (_, p) => comments::describe(d, p),
                };
                let mut v = vec![Span::styled("  ✎ ", Style::new().fg(Color::Yellow))];
                match p {
                    Placement::Stale => v.push("⚠ stale ".fg(Color::Red).bold()),
                    Placement::Line { moved: true, .. } => v.push("moved ".fg(Color::Yellow)),
                    _ => {}
                }
                v.push(format!("{at}  ").fg(ACCENT));
                v.push(first_line(&draft.body).into());
                if draft.remote_id.is_some() {
                    v.push("  (draft note created, not published)".dim());
                }
                v
            }
            PanelRow::Discussion(di) => {
                let disc = &discussions[di];
                let first = &disc.notes[0];
                let state = if disc.is_resolved() {
                    " · resolved".dim()
                } else if disc.is_resolvable() {
                    " · unresolved".fg(Color::Yellow)
                } else {
                    "".into()
                };
                let at = match disc.position() {
                    Some(p) => format!(
                        "  {}:{}",
                        p.new_path,
                        p.new_line
                            .or(p.old_line)
                            .map_or("file".into(), |n| n.to_string())
                    ),
                    None => "  general".into(),
                };
                let replies = disc.notes.iter().skip(1).filter(|n| !n.system).count();
                let mut v = vec![
                    Span::styled("  ◆ ", Style::new().fg(Color::Magenta)),
                    format!("@{}", first.author.username).fg(Color::Magenta),
                    state,
                    at.fg(ACCENT),
                    "  ".into(),
                    first_line(&first.body).into(),
                ];
                if replies > 0 {
                    v.push(format!("  (+{replies})").dim());
                }
                v
            }
        };
        let used: usize = spans.iter().map(Span::width).sum();
        if used < width {
            spans.push(" ".repeat(width - used).into());
        }
        let mut line = Line::from(spans);
        if i == app.panel.sel {
            line = line.patch_style(Style::new().bg(CURSOR_BG));
        }
        lines.push(line);
    }
    let inner_h = rect.height.saturating_sub(2) as usize;
    app.panel.height = inner_h;
    let scroll = sel_line.saturating_sub(inner_h.saturating_sub(2)) as u16;
    let hint = match &app.panel.confirm {
        Some(Confirm::Delete(_)) => {
            Line::from(" Delete this draft?  y / n ".fg(Color::Yellow).bold())
        }
        _ => Line::from(
            " Enter go / edit summary · e edit (reply on a discussion) · t thread · d delete · S summary · p publish · r refresh · Esc close ",
        )
        .dim(),
    };
    let title = format!(
        " review · {} · {} draft(s){} ",
        sh.review.title(),
        sh.comments.drafts.len(),
        if sh.notes.stale > 0 {
            format!(", {} stale", sh.notes.stale)
        } else {
            String::new()
        }
    );
    f.render_widget(Clear, rect);
    f.render_widget(
        Paragraph::new(lines)
            .scroll((scroll, 0))
            .block(popup_block(title).title_bottom(hint)),
        rect,
    );
}

fn render_publish(f: &mut Frame, rect: Rect, app: &App) {
    let Some(Confirm::Publish { plan, json, scroll }) = &app.panel.confirm else {
        return;
    };
    let sh = &app.sh;
    let width = rect.width.saturating_sub(4) as usize;
    let (project, iid) = match &sh.review.mr {
        Some(mr) => (encode(&mr.project), mr.iid.to_string()),
        None => (":project".into(), ":iid".into()),
    };
    let requests = plan.requests(&project, &iid);
    let version = sh
        .remote
        .as_ref()
        .map_or("?".to_string(), |r| r.capabilities.version.clone());
    let mut lines = vec![Line::from(
        format!(
            " Publish {} comment(s) to {} (GitLab {version})?",
            plan.comments.len(),
            sh.review.title()
        )
        .bold(),
    )];
    match plan.mode {
        Mode::Drafts => lines.push(Line::from(
            " Draft notes, then one bulk publish: the review appears at once.".dim(),
        )),
        Mode::Direct => lines.push(Line::from(
            " ⚠ This GitLab has no draft notes: each comment is posted right away, one by one, and notifies at once."
                .fg(Color::Yellow)
                .bold(),
        )),
    }
    if let Some(r) = &sh.remote
        && plan.mode == Mode::Drafts
        && r.pending_drafts > 0
    {
        lines.push(Line::from(
            format!(
                " ⚠ bulk publish also publishes your {} pending draft note(s) already on GitLab.",
                r.pending_drafts
            )
            .fg(Color::Yellow),
        ));
    }
    if !plan.skipped.is_empty() {
        lines.push(Line::from(
            format!(
                " ⚠ {} stale draft(s) not sent (their line is gone).",
                plan.skipped.len()
            )
            .fg(Color::Yellow),
        ));
    }
    lines.push(Line::default());
    let mut body: Vec<Line> = Vec::new();
    if *json {
        for r in &requests {
            body.push(Line::from(format!(" {} {}", r.method, r.path).fg(ACCENT)));
            if let Some(b) = &r.body {
                let pretty = format!("{b:#}");
                body.extend(pretty.lines().map(|l| Line::from(format!("   {l}"))));
            }
        }
    } else {
        for (i, c) in plan.comments.iter().enumerate() {
            let done = if c.remote_id.is_some() {
                "  (already created)"
            } else {
                ""
            };
            body.push(Line::from(vec![
                format!(" {:>3}. ", i + 1).dim(),
                format!("{:<34}", truncate_right(&c.what, 34)).fg(ACCENT),
                truncate_right(&first_line(&c.comment.body), width.saturating_sub(42)).into(),
                done.dim(),
            ]));
        }
        body.push(Line::default());
        for r in &requests {
            body.push(Line::from(format!("  {} {}", r.method, r.path).dim()));
        }
    }
    let room = rect.height.saturating_sub(2) as usize - lines.len().min(rect.height as usize);
    let scroll = (*scroll).min(body.len().saturating_sub(room));
    lines.extend(body.into_iter().skip(scroll));
    f.render_widget(Clear, rect);
    f.render_widget(
        Paragraph::new(lines).block(
            popup_block(" publish the review ".into()).title_bottom(
                Line::from(" y publish · n cancel · J exact JSON requests · j/k scroll ")
                    .fg(Color::Yellow),
            ),
        ),
        rect,
    );
}

// ----- thread ---------------------------------------------------------------

/// Lines of code around the line a note is about, the line itself marked.
fn thread_code(app: &mut App, hunk: usize, line: usize, width: usize) -> Vec<Line<'static>> {
    let d = &app.sh.review.diff;
    let lines = &d.hunks[hunk].lines;
    let from = line.saturating_sub(3);
    let rows: Vec<_> = (from..=line.min(lines.len().saturating_sub(1)))
        .map(|i| (i, lines[i].kind, lines[i].old_line, lines[i].new_line))
        .collect();
    let hl = app.sh.highlighter.hunk(d, hunk);
    let mut out = Vec::new();
    for (i, kind, old, new) in rows {
        let (sign, bg) = match kind {
            LineKind::Added => ("+", Some(ADDED_BG)),
            LineKind::Removed => ("-", Some(REMOVED_BG)),
            LineKind::Context => (" ", None),
        };
        let no = new.or(old).map(|n| n.to_string()).unwrap_or_default();
        let mut spans = vec![
            Span::styled(
                if i == line { "▶" } else { " " },
                Style::new().fg(ACCENT).bold(),
            ),
            format!("{no:>5} ").dim(),
            Span::raw(sign),
        ];
        spans.extend(hl[i].iter().map(|(st, t)| Span::styled(t.clone(), *st)));
        let mut l = super::graph::clip(spans, width);
        if let Some(bg) = bg {
            l = l.style(Style::new().bg(bg));
        }
        out.push(l);
    }
    out
}

/// A note of the thread: its head, then its rendered body.
fn thread_note(
    out: &mut Vec<Line<'static>>,
    head: Vec<Span<'static>>,
    body: &str,
    tone: Tone,
    width: usize,
    app: &App,
) {
    out.push(Line::from(head));
    let body = markdown::render(
        body,
        width.saturating_sub(4),
        tone.body(),
        Some(&app.sh.highlighter),
    );
    out.extend(body.into_iter().map(|mut l| {
        l.spans.insert(0, Span::raw("   "));
        l
    }));
}

pub fn render_thread(f: &mut Frame, area: Rect, app: &mut App, v: &mut ThreadView) {
    let rect = centered(
        area,
        (area.width * 9 / 10).clamp(40, 120),
        (area.height * 9 / 10).max(10),
    );
    let width = rect.width.saturating_sub(2) as usize;
    let placed = app.sh.notes.find(v.kind);
    let at = placed.and_then(|i| app.sh.notes.items[i as usize].at);
    let order = app.sh.notes.ordered(&app.sh.review.diff);
    let rank = placed
        .and_then(|i| order.iter().position(|&x| x == i))
        .map(|p| format!(" {}/{} ", p + 1, order.len()))
        .unwrap_or_default();

    let mut lines: Vec<Line<'static>> = Vec::new();
    if let Some((h, l)) = at {
        lines.extend(thread_code(app, h, l, width));
        lines.push(Line::default());
    }
    let d = &app.sh.review.diff;
    let sep = || Line::from("─".repeat(width.saturating_sub(2)).dim());
    let (title, write) = match v.kind {
        NoteKind::Remote(di) => {
            let Some(disc) = app.sh.discussions().get(di) else {
                return;
            };
            let resolved = disc.is_resolved();
            let tone = if resolved {
                Tone::Resolved
            } else {
                Tone::Remote
            };
            let where_ = match disc.position() {
                Some(p) => format!(
                    "{}:{}",
                    p.new_path,
                    p.new_line
                        .or(p.old_line)
                        .map_or("file".into(), |n| n.to_string())
                ),
                None => "general".into(),
            };
            for (i, n) in disc.notes.iter().filter(|n| !n.system).enumerate() {
                if i > 0 {
                    lines.push(sep());
                }
                let mut head = vec![
                    Span::styled(
                        if i == 0 { " ◆ " } else { " ↳ " },
                        Style::new().fg(tone.bar()),
                    ),
                    format!("@{}", n.author.username).fg(Color::Magenta).bold(),
                ];
                if i == 0 && disc.is_resolvable() {
                    head.push(if resolved {
                        "  resolved".dim()
                    } else {
                        "  ● unresolved".fg(Color::Yellow)
                    });
                }
                if !n.created_at.is_empty() {
                    let date = n
                        .created_at
                        .get(..16)
                        .unwrap_or(&n.created_at)
                        .replace('T', " ");
                    head.push(format!("  {date}").dim());
                }
                thread_note(&mut lines, head, &n.body, tone, width, app);
            }
            // Replies drafted here, not published yet.
            for draft in &app.sh.comments.drafts {
                if matches!(&draft.anchor, comments::Anchor::Reply { discussion, .. } if *discussion == disc.id)
                {
                    lines.push(sep());
                    let head = vec![
                        " ✎ ".fg(Color::Yellow),
                        "draft reply".fg(Color::Yellow).bold(),
                    ];
                    thread_note(&mut lines, head, &draft.body, Tone::Draft, width, app);
                }
            }
            (format!(" thread · {where_} "), "c reply")
        }
        NoteKind::Draft(id) => {
            let Some(draft) = app.sh.comments.get(id) else {
                return;
            };
            let at = comments::describe(d, comments::place(&draft.anchor, d));
            let head = vec![" ✎ ".fg(Color::Yellow), "draft".fg(Color::Yellow).bold()];
            thread_note(&mut lines, head, &draft.body, Tone::Draft, width, app);
            (format!(" draft · {at} "), "c edit")
        }
    };
    let inner_h = rect.height.saturating_sub(2) as usize;
    v.height = inner_h;
    v.total = lines.len();
    v.scroll = v.scroll.min(lines.len().saturating_sub(inner_h));
    let hint = format!(" {write} · n/N next thread · j/k scroll · Esc close ");
    f.render_widget(Clear, rect);
    f.render_widget(
        Paragraph::new(lines).scroll((v.scroll as u16, 0)).block(
            popup_block(title)
                .title(Line::from(rank).right_aligned().dim())
                .title_bottom(Line::from(hint).dim()),
        ),
        rect,
    );
}
