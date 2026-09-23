//! Questions to the LLM: the question input with suggestions, the answer
//! with its navigable `[path:line]` links, and the history of the review's
//! questions. Rendering lives in `ui/ask.rs`.

use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use survol_core::ask::{Answer, CodeRef, Subject};

/// The question being typed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AskInput {
    pub subject: Subject,
    pub label: String,
    pub text: String,
    /// Suggestion copied into `text` by ↑ / ↓.
    pub suggestion: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputOutcome {
    Continue,
    Cancel,
    Submit(String),
}

impl AskInput {
    pub fn new(subject: Subject, label: String) -> Self {
        Self {
            subject,
            label,
            text: String::new(),
            suggestion: None,
        }
    }

    pub fn suggestions(&self) -> &'static [&'static str] {
        self.subject.suggestions()
    }

    fn pick(&mut self, forward: bool) {
        let n = self.suggestions().len();
        if n == 0 {
            return;
        }
        let i = match (self.suggestion, forward) {
            (None, true) => 0,
            (None, false) => n - 1,
            (Some(i), true) => (i + 1) % n,
            (Some(i), false) => (i + n - 1) % n,
        };
        self.suggestion = Some(i);
        self.text = self.suggestions()[i].to_string();
    }

    pub fn on_key(&mut self, key: KeyEvent) -> InputOutcome {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => return InputOutcome::Cancel,
            KeyCode::Enter => {
                let q = self.text.trim();
                if !q.is_empty() {
                    return InputOutcome::Submit(q.to_string());
                }
            }
            KeyCode::Down | KeyCode::Tab => self.pick(true),
            KeyCode::Up | KeyCode::BackTab => self.pick(false),
            KeyCode::Char('n') if ctrl => self.pick(true),
            KeyCode::Char('p') if ctrl => self.pick(false),
            KeyCode::Char('u') if ctrl => self.text.clear(),
            KeyCode::Char(c @ '1'..='9') if self.text.is_empty() => {
                let i = c as usize - '1' as usize;
                if let Some(s) = self.suggestions().get(i) {
                    return InputOutcome::Submit(s.to_string());
                }
            }
            KeyCode::Backspace => {
                self.text.pop();
                self.suggestion = None;
            }
            KeyCode::Char(c) if !ctrl => {
                self.text.push(c);
                self.suggestion = None;
            }
            _ => {}
        }
        InputOutcome::Continue
    }
}

/// A question on its way to the LLM.
#[derive(Debug, Clone)]
pub struct Pending {
    pub label: String,
    pub question: String,
    pub since: Instant,
}

/// An answer on screen.
#[derive(Debug, Clone)]
pub struct AnswerView {
    pub answer: Answer,
    pub scroll: usize,
    /// Selected reference (index into `answer.refs`, valid ones only).
    pub link: Option<usize>,
    /// Updated by the renderer.
    pub height: usize,
    pub total_lines: usize,
    /// Line of the selected link, set by the renderer to keep it visible.
    pub link_line: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnswerOutcome {
    Continue,
    Close,
    /// Follow the reference; `diff`: to the Diff view rather than the graph.
    Follow {
        r: CodeRef,
        diff: bool,
    },
    Edit(CodeRef),
    History,
}

impl AnswerView {
    pub fn new(answer: Answer) -> Self {
        let mut v = Self {
            answer,
            scroll: 0,
            link: None,
            height: 10,
            total_lines: 0,
            link_line: None,
        };
        v.link = v.valid_links().first().copied();
        v
    }

    fn valid_links(&self) -> Vec<usize> {
        (0..self.answer.refs.len())
            .filter(|&i| self.answer.refs[i].valid)
            .collect()
    }

    fn next_link(&mut self, forward: bool) {
        let links = self.valid_links();
        if links.is_empty() {
            return;
        }
        let pos = self.link.and_then(|l| links.iter().position(|&x| x == l));
        let n = links.len();
        let i = match (pos, forward) {
            (None, _) => 0,
            (Some(p), true) => (p + 1) % n,
            (Some(p), false) => (p + n - 1) % n,
        };
        self.link = Some(links[i]);
        self.link_line = None;
    }

    fn scroll_by(&mut self, delta: isize) {
        let max = self.total_lines.saturating_sub(self.height);
        self.scroll = self.scroll.saturating_add_signed(delta).min(max);
    }

    pub fn selected(&self) -> Option<&CodeRef> {
        self.link.and_then(|l| self.answer.refs.get(l))
    }

    pub fn on_key(&mut self, key: KeyEvent) -> AnswerOutcome {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let half = (self.height / 2).max(1) as isize;
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return AnswerOutcome::Close,
            KeyCode::Char('j') | KeyCode::Down => self.scroll_by(1),
            KeyCode::Char('k') | KeyCode::Up => self.scroll_by(-1),
            KeyCode::Char('d') if ctrl => self.scroll_by(half),
            KeyCode::Char('u') if ctrl => self.scroll_by(-half),
            KeyCode::PageDown => self.scroll_by(half * 2),
            KeyCode::PageUp => self.scroll_by(-half * 2),
            KeyCode::Tab | KeyCode::Char('n') => self.next_link(true),
            KeyCode::BackTab | KeyCode::Char('N') => self.next_link(false),
            KeyCode::Char('A') => return AnswerOutcome::History,
            KeyCode::Enter | KeyCode::Char('d') | KeyCode::Char('e') => {
                if let Some(r) = self.selected().cloned() {
                    return match key.code {
                        KeyCode::Char('e') => AnswerOutcome::Edit(r),
                        KeyCode::Char('d') => AnswerOutcome::Follow { r, diff: true },
                        _ => AnswerOutcome::Follow { r, diff: false },
                    };
                }
            }
            _ => {}
        }
        AnswerOutcome::Continue
    }
}

/// The questions of the review, newest first.
#[derive(Debug, Clone, Default)]
pub struct HistoryView {
    pub sel: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HistoryOutcome {
    Continue,
    Close,
    /// Index into the history (oldest first).
    Open(usize),
}

impl HistoryView {
    pub fn on_key(&mut self, key: KeyEvent, len: usize) -> HistoryOutcome {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('A') => return HistoryOutcome::Close,
            KeyCode::Char('j') | KeyCode::Down => {
                self.sel = (self.sel + 1).min(len.saturating_sub(1))
            }
            KeyCode::Char('k') | KeyCode::Up => self.sel = self.sel.saturating_sub(1),
            KeyCode::Enter if len > 0 => {
                return HistoryOutcome::Open(len - 1 - self.sel.min(len - 1));
            }
            _ => {}
        }
        HistoryOutcome::Continue
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use survol_core::model::Side;

    fn key(c: KeyCode) -> KeyEvent {
        KeyEvent::new(c, KeyModifiers::NONE)
    }

    fn cref(line: u32, valid: bool) -> CodeRef {
        CodeRef {
            start: 0,
            end: 1,
            raw_path: "a.java".into(),
            path: "src/a.java".into(),
            line,
            end_line: None,
            side: Side::New,
            valid,
        }
    }

    fn answer(refs: Vec<CodeRef>) -> Answer {
        Answer {
            subject: Subject::Group(1),
            label: "group 1. X".into(),
            question: "What?".into(),
            text: "x".into(),
            refs,
            model: None,
            head_sha: "h".into(),
            prompt_version: 1,
            key: "k".into(),
            asked_at: 0,
            from_cache: false,
        }
    }

    #[test]
    fn input_suggestions_and_typing() {
        let mut i = AskInput::new(Subject::Symbol("s".into()), "Owner.addPet".into());
        assert_eq!(i.on_key(key(KeyCode::Enter)), InputOutcome::Continue);
        i.on_key(key(KeyCode::Down));
        assert_eq!(i.text, "What does this component do?");
        i.on_key(key(KeyCode::Down));
        assert_eq!(i.suggestion, Some(1));
        i.on_key(key(KeyCode::Backspace));
        assert_eq!(i.suggestion, None);
        i.text.clear();
        // A digit on an empty input asks that suggestion.
        assert_eq!(
            i.on_key(key(KeyCode::Char('2'))),
            InputOutcome::Submit("Why does the code go through here?".into())
        );
        for c in "Who calls it?".chars() {
            i.on_key(key(KeyCode::Char(c)));
        }
        assert_eq!(
            i.on_key(key(KeyCode::Enter)),
            InputOutcome::Submit("Who calls it?".into())
        );
        assert_eq!(i.on_key(key(KeyCode::Esc)), InputOutcome::Cancel);
    }

    #[test]
    fn answer_links_skip_unknown_references() {
        let mut v = AnswerView::new(answer(vec![cref(1, true), cref(2, false), cref(3, true)]));
        assert_eq!(v.link, Some(0));
        v.on_key(key(KeyCode::Tab));
        assert_eq!(v.link, Some(2));
        v.on_key(key(KeyCode::Tab));
        assert_eq!(v.link, Some(0));
        v.on_key(key(KeyCode::BackTab));
        assert_eq!(v.link, Some(2));
        assert_eq!(
            v.on_key(key(KeyCode::Enter)),
            AnswerOutcome::Follow {
                r: cref(3, true),
                diff: false
            }
        );
        assert_eq!(
            v.on_key(key(KeyCode::Char('e'))),
            AnswerOutcome::Edit(cref(3, true))
        );
        let mut none = AnswerView::new(answer(vec![cref(2, false)]));
        assert_eq!(none.link, None);
        assert_eq!(none.on_key(key(KeyCode::Enter)), AnswerOutcome::Continue);
        assert_eq!(none.on_key(key(KeyCode::Esc)), AnswerOutcome::Close);
    }

    #[test]
    fn history_opens_newest_first() {
        let mut h = HistoryView::default();
        assert_eq!(h.on_key(key(KeyCode::Enter), 3), HistoryOutcome::Open(2));
        h.on_key(key(KeyCode::Char('j')), 3);
        assert_eq!(h.on_key(key(KeyCode::Enter), 3), HistoryOutcome::Open(1));
        assert_eq!(h.on_key(key(KeyCode::Enter), 0), HistoryOutcome::Continue);
    }
}
