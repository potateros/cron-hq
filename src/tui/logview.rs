//! The log view (port of `LogView.tsx`): a full-screen tail with a line cursor.
//! Following (the default) keeps the cursor on the newest line; moving up detaches it.
//! ⏎ or space expands the cursor line, `w` wraps every line.

use super::canvas::{between, bold, put, rule, sp, visible};
use super::theme::{inner_width, truncate_width, ACCENT, BODY, FAULT, HOLD, MARGIN};
use super::{App, Hit, Mode};
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::Rect;
use ratatui::Frame;
use std::collections::HashSet;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use unicode_width::UnicodeWidthChar;

pub const KEYS: &str = "↑↓ line   PgUp PgDn page   ⏎ expand   w wrap all   g G top/live   esc back";

/// Width of the "▸ " cursor gutter in front of every line.
const GUTTER: usize = 2;

/// What a log view shows and where it is in the file. A fresh one is made every time a log is opened.
#[derive(Debug, Default)]
pub struct LogState {
    /// Decoded lines; the last one may be unfinished (see `pending`).
    pub lines: Vec<String>,
    /// `None` = following the tail.
    pub cursor: Option<usize>,
    pub expanded: HashSet<usize>,
    pub wrap_all: bool,
    /// First line of the viewport at the last draw.
    pub top: usize,
    /// Bytes of the file consumed so far; the next read starts here.
    pub len_seen: u64,
    /// Raw bytes of an unfinished last line, kept until its newline (or the rest of a split character) arrives.
    pub pending: Vec<u8>,
    pub path: Option<PathBuf>,
    /// Rows the viewport had at the last draw: a page.
    pub height: usize,
}

/// Longest prefix of `s` that fits `width` columns, but at least one char, and the rest.
fn split_width(s: &str, width: usize) -> (&str, &str) {
    let mut w = 0;
    for (i, c) in s.char_indices() {
        let cw = c.width().unwrap_or(0);
        if w + cw > width && i > 0 {
            return (&s[..i], &s[i..]);
        }
        w += cw;
    }
    (s, "")
}

/// Hard-wrap `line` into rows of at most `width` display columns. Always at least one row.
pub fn chunk(line: &str, width: usize) -> Vec<String> {
    use unicode_width::UnicodeWidthStr;
    if line.width() <= width {
        return vec![line.to_string()];
    }
    let mut rows = Vec::new();
    let mut rest = line;
    while !rest.is_empty() {
        let (head, tail) = split_width(rest, width);
        rows.push(head.to_string());
        rest = tail;
    }
    rows
}

/// One line as it is drawn: the full wrap when expanded, otherwise one row cut with `…`.
pub fn rows_for(line: &str, width: usize, expanded: bool) -> Vec<String> {
    let rows = chunk(line, width);
    if expanded || rows.len() == 1 {
        rows
    } else {
        vec![truncate_width(line, width)]
    }
}

/// First line of the viewport, so that `cursor` is on screen.
/// `anchor_bottom` packs as many lines as fit above the cursor (following the tail);
/// otherwise it moves `start` as little as possible from where it was.
pub fn view_top(heights: &[usize], cursor: usize, height: usize, start: usize, anchor_bottom: bool) -> usize {
    if heights.is_empty() {
        return 0;
    }
    let cursor = cursor.min(heights.len() - 1);
    if anchor_bottom {
        let mut top = cursor;
        let mut used = heights[cursor];
        while top > 0 && used + heights[top - 1] <= height {
            top -= 1;
            used += heights[top];
        }
        return top;
    }
    let mut top = start.min(cursor);
    let mut used: usize = heights[top..=cursor].iter().sum();
    while top < cursor && used > height {
        used -= heights[top];
        top += 1;
    }
    top
}

impl LogState {
    /// A fresh, following state for the log at `path`, with the file read in.
    pub fn open(path: Option<PathBuf>) -> LogState {
        let mut s = LogState { path, height: 19, ..LogState::default() };
        s.sync();
        s
    }

    /// The line the cursor is on (the last one while following).
    pub fn selected(&self) -> usize {
        let last = self.lines.len().saturating_sub(1);
        self.cursor.map_or(last, |c| c.min(last))
    }

    /// Move the cursor to line `i` (negative = the top); the last line or beyond follows the tail.
    fn move_to(&mut self, i: isize) {
        let last = self.lines.len().saturating_sub(1);
        self.cursor = if i >= last as isize { None } else { Some(i.max(0) as usize) };
    }

    /// A click on line `i`.
    pub fn select(&mut self, i: usize) {
        self.move_to(i as isize);
    }

    /// ↑↓ PgUp PgDn g G ⏎ space w. (Esc is the app's.)
    pub fn handle_key(&mut self, ev: KeyEvent) {
        if self.lines.is_empty() {
            return;
        }
        let at = self.selected() as isize;
        let page = self.height.max(1) as isize;
        match ev.code {
            KeyCode::Up => self.move_to(at - 1),
            KeyCode::Down => self.move_to(at + 1),
            KeyCode::PageUp => self.move_to(at - page),
            KeyCode::PageDown => self.move_to(at + page),
            KeyCode::Char('g') => {
                self.top = 0;
                self.move_to(0);
            }
            KeyCode::Char('G') => self.cursor = None,
            KeyCode::Enter | KeyCode::Char(' ') => {
                let at = at as usize;
                if !self.expanded.remove(&at) {
                    self.expanded.insert(at);
                }
            }
            KeyCode::Char('w') => self.wrap_all = !self.wrap_all,
            _ => {}
        }
    }

    /// Start over from an empty file (the log rotated or was removed).
    fn reset(&mut self) {
        self.lines.clear();
        self.pending.clear();
        self.len_seen = 0;
        self.cursor = None;
        self.expanded.clear();
        self.top = 0;
    }

    /// Add raw bytes after what was already read. Only finished lines are decoded for good;
    /// an unfinished one is shown and decoded again when more arrives.
    fn append(&mut self, bytes: &[u8]) {
        let mut data = std::mem::take(&mut self.pending);
        if !data.is_empty() {
            self.lines.pop();
        }
        data.extend_from_slice(bytes);
        let mut parts = data.split(|b| *b == b'\n').peekable();
        while let Some(part) = parts.next() {
            if parts.peek().is_some() {
                self.lines.push(String::from_utf8_lossy(part).into_owned());
            } else if !part.is_empty() {
                self.lines.push(String::from_utf8_lossy(part).into_owned());
                self.pending = part.to_vec();
            }
        }
    }

    /// Stat the file and read only what was appended since last time; re-read from the start if it shrank.
    /// Returns whether anything changed.
    pub fn sync(&mut self) -> bool {
        let Some(path) = self.path.clone() else { return false };
        let len = std::fs::metadata(&path).map_or(0, |m| m.len());
        if len == self.len_seen {
            return false;
        }
        if len < self.len_seen {
            self.reset();
        }
        if len > self.len_seen {
            let mut buf = Vec::new();
            let read = File::open(&path).and_then(|mut f| {
                f.seek(SeekFrom::Start(self.len_seen))?;
                f.take(len - self.len_seen).read_to_end(&mut buf)
            });
            if read.is_ok() {
                self.append(&buf);
                self.len_seen += buf.len() as u64;
            }
        }
        true
    }
}

impl App {
    /// Open the log of job `id`: always a fresh state, following the tail.
    pub fn open_log(&mut self, id: String) {
        let path = self.paths.as_ref().map(|p| p.log(&id));
        self.log = LogState::open(path);
        self.mode = Mode::Log(id);
    }

    /// Pick up what the job wrote since the last look. True when the screen needs a redraw.
    pub fn refresh_log(&mut self) -> bool {
        matches!(self.mode, Mode::Log(_)) && self.log.sync()
    }
}

fn line_color(line: &str) -> ratatui::style::Color {
    if line.starts_with("── RUN") {
        ACCENT
    } else if line.starts_with("── END") {
        if line.contains("NOMINAL") {
            ACCENT
        } else {
            FAULT
        }
    } else {
        BODY
    }
}

/// The clickable parts of the footer: (offset in the segment, width, key). `↑↓` is two keys, so it is not one.
fn segment_targets(seg: &str) -> Vec<(u16, u16, KeyCode)> {
    use unicode_width::UnicodeWidthStr;
    let whole = seg.width() as u16;
    match seg.split(' ').next().unwrap_or("") {
        "PgUp" => vec![(0, 4, KeyCode::PageUp), (5, 4, KeyCode::PageDown)],
        "g" => vec![(0, 1, KeyCode::Char('g')), (2, 1, KeyCode::Char('G'))],
        "⏎" => vec![(0, whole, KeyCode::Enter)],
        "w" => vec![(0, whole, KeyCode::Char('w'))],
        "esc" => vec![(0, whole, KeyCode::Esc)],
        _ => Vec::new(),
    }
}

/// Draw the log of job `id` and register a `Hit::LogLine` for every visible row, wrapped continuations included.
pub fn draw_log(frame: &mut Frame, log: &mut LogState, id: &str, hits: &mut Vec<(Rect, Hit)>) {
    use unicode_width::UnicodeWidthStr;
    let area = frame.area();
    let w = inner_width(area.width);
    let height = (area.height as usize).saturating_sub(5).max(5);
    let text_w = (w as usize).saturating_sub(GUTTER).max(10);
    log.height = height;

    // The log rotated: line numbers no longer mean the same lines.
    let last = log.lines.len().saturating_sub(1);
    if log.cursor.is_some_and(|c| c > last) {
        log.cursor = None;
        log.expanded.clear();
    }
    let at = log.selected();
    let wrap_all = log.wrap_all;
    let rows_of = |log: &LogState, i: usize| rows_for(&log.lines[i], text_w, wrap_all || log.expanded.contains(&i));

    // Only the lines that can reach the viewport are measured: at most `height` above the cursor.
    let top = if log.lines.is_empty() {
        0
    } else {
        let lo = at.saturating_sub(height);
        let heights: Vec<usize> = (lo..=at).map(|i| rows_of(log, i).len()).collect();
        let start = log.top.clamp(lo, at) - lo;
        lo + view_top(&heights, at - lo, height, start, log.cursor.is_none())
    };
    log.top = top;

    let status = [if log.cursor.is_none() { "LIVE".to_string() } else { format!("{}/{}", at + 1, log.lines.len()) }]
        .into_iter()
        .chain(wrap_all.then(|| "WRAP".to_string()))
        .collect::<Vec<_>>()
        .join(" · ");

    let buf = frame.buffer_mut();
    let mut y: u16 = 1; // paddingTop
    between(buf, area, MARGIN, y, w, &[bold(sp(format!("◢ H Q · LOG · {}", id.to_uppercase()), ACCENT))], &[sp(status, HOLD)]);
    y += 1;
    rule(buf, area, y, w);
    y += 1;

    if log.lines.is_empty() {
        put(buf, area, MARGIN, y, w, &[sp("no log yet", HOLD)]);
    } else {
        let mut shown = 0;
        let mut i = top;
        while i < log.lines.len() && shown < height {
            let color = line_color(&log.lines[i]);
            for (j, text) in rows_of(log, i).into_iter().enumerate() {
                if shown >= height {
                    break;
                }
                let row_y = y + shown as u16;
                let mark = if i == at && j == 0 { "▸ " } else { "  " };
                let text = if text.is_empty() { " ".to_string() } else { text };
                let mut body = sp(text, color);
                if i == at {
                    body = bold(body);
                }
                put(buf, area, MARGIN, row_y, w, &[sp(mark, ACCENT), body]);
                if let Some(r) = visible(Rect::new(MARGIN, row_y, w, 1), area) {
                    hits.push((r, Hit::LogLine(i)));
                }
                shown += 1;
            }
            i += 1;
        }
    }
    y += height as u16;
    rule(buf, area, y, w);
    y += 1;
    put(buf, area, MARGIN, y, w, &[sp(KEYS, HOLD)]);
    let mut x = MARGIN;
    for seg in KEYS.split("   ") {
        for (off, width, code) in segment_targets(seg) {
            let right = (MARGIN + w).saturating_sub(x + off);
            if let Some(r) = visible(Rect::new(x + off, y, width.min(right), 1), area) {
                hits.push((r, Hit::Key(code)));
            }
        }
        x += seg.width() as u16 + 3;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::Paths;
    use crate::tui::testing::{find, lines, snapshot, Rec};
    use crate::tui::{draw, App, Mode};
    use chrono::Local;
    use crossterm::event::{KeyEventKind, KeyEventState, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use std::io::Write;

    const LONG_TAIL: &str = "TAIL-MARKER";
    fn long_line() -> String {
        format!("pushed at-a-glance (battery 3322 mV, signal -55 dBm, clock drift -4s), fallback armed for midnight {} {LONG_TAIL}", "x".repeat(80))
    }

    fn k(c: KeyCode) -> KeyEvent {
        KeyEvent { code: c, modifiers: KeyModifiers::NONE, kind: KeyEventKind::Press, state: KeyEventState::NONE }
    }
    fn click(col: u16, row: u16) -> MouseEvent {
        MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column: col, row, modifiers: KeyModifiers::NONE }
    }
    fn wheel(up: bool) -> MouseEvent {
        let kind = if up { MouseEventKind::ScrollUp } else { MouseEventKind::ScrollDown };
        MouseEvent { kind, column: 0, row: 0, modifiers: KeyModifiers::NONE }
    }

    /// An app on the jobs view whose first job ("eink-weather") has `content` as its log.
    fn fixture(content: &[u8]) -> (App, Rec, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::temp(dir.path());
        std::fs::create_dir_all(paths.home.join("logs")).unwrap();
        std::fs::write(paths.log("eink-weather"), content).unwrap();
        let mut a = App::new(Local::now());
        a.snap = Some(snapshot());
        a.paths = Some(paths);
        (a, Rec::default(), dir)
    }
    fn standard() -> Vec<u8> {
        format!("── RUN 0001 · 2026-10-17 17:28:48 · scheduled ──\n{}\n── END 0001 · NOMINAL · 54s · exit 0 ──\n", long_line()).into_bytes()
    }
    fn open(content: &[u8]) -> (App, Rec, tempfile::TempDir) {
        let (mut a, fx, d) = fixture(content);
        a.handle_key(k(KeyCode::Enter), &fx);
        assert!(matches!(&a.mode, Mode::Log(id) if id == "eink-weather"));
        (a, fx, d)
    }
    fn render_size(a: &mut App, w: u16, h: u16) -> Vec<String> {
        let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
        t.draw(|f| draw(f, a)).unwrap();
        lines(t.backend().buffer())
    }
    fn render(a: &mut App) -> Vec<String> {
        render_size(a, 100, 30)
    }
    fn text(a: &mut App) -> String {
        render(a).join("\n")
    }

    // ── chunk / rows_for ────────────────────────────────────────────────────

    #[test]
    fn short_lines_are_one_row() {
        assert_eq!(chunk("abc", 5), ["abc"]);
        assert_eq!(rows_for("abc", 5, false), ["abc"]);
    }

    #[test]
    fn long_lines_wrap_when_expanded_and_are_cut_otherwise() {
        assert_eq!(chunk("abcdefghij", 4), ["abcd", "efgh", "ij"]);
        assert_eq!(rows_for("abcdefghij", 4, true), ["abcd", "efgh", "ij"]);
        assert_eq!(rows_for("abcdefghij", 4, false), ["abc…"]);
    }

    #[test]
    fn counts_display_columns_not_bytes() {
        assert_eq!(chunk("──────", 3), ["───", "───"]);
        assert_eq!(chunk("日本語", 4), ["日本", "語"]);
        assert_eq!(chunk("a日b", 2), ["a", "日", "b"], "a wide char never straddles rows");
        assert_eq!(chunk("日", 1), ["日"], "a char wider than the row still advances");
        assert_eq!(chunk("abc", 0), ["a", "b", "c"], "width 0 does not loop forever");
        assert_eq!(rows_for("日本語日本語", 5, false), ["日本…"]);
    }

    // ── view_top ────────────────────────────────────────────────────────────

    #[test]
    fn bottom_anchor_packs_lines_above_the_cursor() {
        assert_eq!(view_top(&[1, 1, 1, 1, 1], 4, 3, 0, true), 2);
        assert_eq!(view_top(&[1, 1, 3, 1], 3, 4, 0, true), 2);
    }

    #[test]
    fn keep_scrolls_only_as_far_as_needed() {
        assert_eq!(view_top(&[1, 1, 1, 1, 1], 1, 3, 0, false), 0);
        assert_eq!(view_top(&[1, 1, 1, 1, 1], 3, 3, 0, false), 1);
        assert_eq!(view_top(&[1, 1, 1, 1, 1], 1, 3, 2, false), 1);
    }

    #[test]
    fn an_expanded_cursor_line_pushes_earlier_lines_off_the_top() {
        assert_eq!(view_top(&[1, 1, 3], 2, 4, 0, false), 1);
    }

    #[test]
    fn a_cursor_line_taller_than_the_screen_stays_at_the_top() {
        assert_eq!(view_top(&[1, 9], 1, 4, 0, false), 1);
    }

    // ── the view ────────────────────────────────────────────────────────────

    #[test]
    fn opens_following_the_tail_with_the_cursor_on_the_last_line() {
        let (mut a, _fx, _d) = open(&standard());
        let f = text(&mut a);
        assert!(f.contains("LIVE"), "{f}");
        assert!(f.contains("▸ ── END 0001"), "{f}");
        assert!(!f.contains(LONG_TAIL));
        assert!(f.contains('…'));
        assert!(f.contains("◢ H Q · LOG · EINK-WEATHER"));
        assert!(f.contains("↑↓ line   PgUp PgDn page   ⏎ expand   w wrap all   g G top/live   esc back"));
    }

    #[test]
    fn enter_on_a_selected_line_expands_it_and_again_collapses_it() {
        let (mut a, fx, _d) = open(&standard());
        a.handle_key(k(KeyCode::Up), &fx);
        let f = text(&mut a);
        assert!(f.contains("2/3"), "{f}");
        assert!(f.contains("▸ pushed at-a-glance"));
        a.handle_key(k(KeyCode::Enter), &fx);
        assert!(text(&mut a).contains(LONG_TAIL));
        a.handle_key(k(KeyCode::Enter), &fx);
        assert!(!text(&mut a).contains(LONG_TAIL));
        a.handle_key(k(KeyCode::Char(' ')), &fx);
        assert!(text(&mut a).contains(LONG_TAIL), "space expands too");
    }

    #[test]
    fn w_wraps_every_line_and_down_past_the_end_returns_to_live() {
        let (mut a, fx, _d) = open(&standard());
        a.handle_key(k(KeyCode::Char('w')), &fx);
        let f = text(&mut a);
        assert!(f.contains(LONG_TAIL));
        assert!(f.contains("LIVE · WRAP"));
        a.handle_key(k(KeyCode::Up), &fx);
        assert!(text(&mut a).contains("2/3 · WRAP"));
        a.handle_key(k(KeyCode::Down), &fx);
        assert!(text(&mut a).contains("LIVE · WRAP"));
    }

    #[test]
    fn g_goes_to_the_top_and_capital_g_back_to_live() {
        let (mut a, fx, _d) = open(&standard());
        a.handle_key(k(KeyCode::Char('g')), &fx);
        assert!(text(&mut a).contains("1/3"));
        a.handle_key(k(KeyCode::PageDown), &fx);
        assert!(text(&mut a).contains("LIVE"), "a page down from the top of a short log reaches the end");
        a.handle_key(k(KeyCode::PageUp), &fx);
        assert!(text(&mut a).contains("1/3"));
        a.handle_key(k(KeyCode::Char('G')), &fx);
        assert!(text(&mut a).contains("LIVE"));
    }

    #[test]
    fn no_log_yet() {
        let (mut a, fx, _d) = fixture(b"");
        std::fs::remove_file(a.paths.as_ref().unwrap().log("eink-weather")).unwrap();
        a.handle_key(k(KeyCode::Enter), &fx);
        let f = text(&mut a);
        assert!(f.contains("no log yet"), "{f}");
        a.handle_key(k(KeyCode::Up), &fx);
        a.handle_key(k(KeyCode::Enter), &fx);
        text(&mut a);
    }

    #[test]
    fn opening_a_log_always_starts_live_with_fresh_state() {
        let (mut a, fx, _d) = open(&standard());
        a.handle_key(k(KeyCode::Char('g')), &fx);
        a.handle_key(k(KeyCode::Char('w')), &fx);
        a.handle_key(k(KeyCode::Esc), &fx);
        assert!(matches!(a.mode, Mode::Dash));
        a.handle_key(k(KeyCode::Enter), &fx);
        let f = text(&mut a);
        assert!(f.contains("LIVE") && !f.contains("WRAP") && !f.contains("1/3"), "{f}");
        // R opens the log too, fresh as well
        a.handle_key(k(KeyCode::Char('g')), &fx);
        a.handle_key(k(KeyCode::Esc), &fx);
        a.handle_key(k(KeyCode::Char('r')), &fx);
        assert!(text(&mut a).contains("LIVE"));
    }

    #[test]
    fn truncates_wide_chars_by_width() {
        let line = format!("{}{}", "─".repeat(100), "😀".repeat(50));
        let (mut a, _fx, _d) = open(format!("{line}\n").as_bytes());
        for w in [100u16, 60, 37, 20] {
            let ls = render_size(&mut a, w, 12);
            let row = ls.iter().find(|l| l.contains("▸")).unwrap();
            assert!(row.contains('…'), "{row}");
            assert!(row.trim_end().ends_with('…'), "{row}");
            assert!(unicode_width::UnicodeWidthStr::width(row.as_str()) <= w as usize);
        }
        // expanded and wrapped: every row fits too, none is lost
        a.log.expanded.insert(0);
        let ls = render_size(&mut a, 40, 20);
        let n = ls.iter().filter(|l| l.contains('😀') || l.contains('─')).count();
        assert!(n >= 4, "{ls:?}");
        for l in &ls {
            assert!(unicode_width::UnicodeWidthStr::width(l.as_str()) <= 40);
        }
    }

    #[test]
    fn renders_tiny_terminals() {
        let (mut a, _fx, _d) = open(&standard());
        a.log.wrap_all = true;
        for (w, h) in [(20, 5), (1, 1), (3, 2), (0, 0), (9, 30)] {
            render_size(&mut a, w, h);
        }
    }

    // ── mouse ───────────────────────────────────────────────────────────────

    #[test]
    fn click_selects_line_then_expands() {
        let (mut a, fx, _d) = open(&standard());
        let ls = render(&mut a);
        let (x, y) = find(&ls, "pushed at-a-glance");
        assert!(a.handle_mouse(click(x, y), &fx));
        let f = text(&mut a);
        assert!(f.contains("2/3") && f.contains("▸ pushed at-a-glance"), "{f}");
        assert!(!f.contains(LONG_TAIL), "first click only selects");
        let ls = render(&mut a);
        let (x, y) = find(&ls, "pushed at-a-glance");
        a.handle_mouse(click(x, y), &fx);
        assert!(text(&mut a).contains(LONG_TAIL), "second click expands");
        let ls = render(&mut a);
        let (x, y) = find(&ls, "pushed at-a-glance");
        a.handle_mouse(click(x, y), &fx);
        assert!(!text(&mut a).contains(LONG_TAIL), "third collapses");
    }

    #[test]
    fn click_on_wrapped_continuation_row_selects_its_line() {
        let (mut a, fx, _d) = open(&standard());
        a.handle_key(k(KeyCode::Char('w')), &fx);
        let ls = render(&mut a);
        let (x, y) = find(&ls, LONG_TAIL);
        assert!(!ls[y as usize].contains("pushed"), "TAIL-MARKER is on a continuation row");
        a.handle_mouse(click(x, y), &fx);
        let f = text(&mut a);
        assert!(f.contains("2/3 · WRAP"), "{f}");
    }

    #[test]
    fn wheel_scrolls_three_lines() {
        let body: String = (1..=20).map(|i| format!("line {i:02}\n")).collect();
        let (mut a, fx, _d) = open(body.as_bytes());
        render(&mut a);
        assert!(a.handle_mouse(wheel(true), &fx));
        assert!(text(&mut a).contains("17/20"), "wheel up = 3 lines up from the tail");
        a.handle_mouse(wheel(true), &fx);
        assert!(text(&mut a).contains("14/20"));
        a.handle_mouse(wheel(false), &fx);
        assert!(text(&mut a).contains("17/20"));
        a.handle_mouse(wheel(false), &fx);
        a.handle_mouse(wheel(false), &fx);
        assert!(text(&mut a).contains("LIVE"), "past the end follows again");
        assert!(matches!(a.mode, Mode::Log(_)));
    }

    #[test]
    fn footer_segments_are_clickable() {
        let (mut a, fx, _d) = open(&standard());
        let ls = render(&mut a);
        let (x, y) = find(&ls, "w wrap all");
        a.handle_mouse(click(x, y), &fx);
        assert!(text(&mut a).contains("LIVE · WRAP"));
        let ls = render(&mut a);
        let (x, y) = find(&ls, "g G top");
        a.handle_mouse(click(x, y), &fx);
        assert!(text(&mut a).contains("1/3"), "g");
        let ls = render(&mut a);
        let (x, y) = find(&ls, "g G top");
        a.handle_mouse(click(x + 2, y), &fx);
        assert!(text(&mut a).contains("LIVE"), "G");
        let (x, y) = find(&ls, "PgUp");
        a.handle_mouse(click(x, y), &fx);
        assert!(!text(&mut a).contains("LIVE"), "PgUp");
        let ls = render(&mut a);
        let (x, y) = find(&ls, "↑↓ line");
        a.handle_mouse(click(x, y), &fx);
        assert!(matches!(a.mode, Mode::Log(_)), "↑↓ is not clickable");
        let (x, y) = find(&ls, "esc back");
        a.handle_mouse(click(x + 2, y), &fx);
        assert!(matches!(a.mode, Mode::Dash), "esc back acts as Esc");
    }

    // ── following ───────────────────────────────────────────────────────────

    #[test]
    fn follows_appended_bytes_and_rereads_after_rotation() {
        let (mut a, fx, _d) = open(b"one\ntwo\n");
        let path = a.paths.as_ref().unwrap().log("eink-weather");
        assert!(!a.refresh_log(), "nothing new");
        assert_eq!(a.log.len_seen, 8);
        let mut f = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
        f.write_all(b"thr").unwrap();
        assert!(a.refresh_log());
        assert_eq!(a.log.lines, ["one", "two", "thr"], "a partial line shows");
        f.write_all(b"ee\nfour\n").unwrap();
        assert!(a.refresh_log());
        assert_eq!(a.log.lines, ["one", "two", "three", "four"]);
        assert_eq!(a.log.len_seen, 19);
        assert!(text(&mut a).contains("▸ four"), "still LIVE");
        // detached cursor keeps its place while the log grows
        a.handle_key(k(KeyCode::Up), &fx);
        f.write_all(b"five\n").unwrap();
        a.refresh_log();
        assert!(text(&mut a).contains("3/5"));
        // rotation: the file shrank
        std::fs::write(&path, b"fresh\n").unwrap();
        assert!(a.refresh_log());
        assert_eq!(a.log.lines, ["fresh"]);
        assert_eq!(a.log.len_seen, 6);
        assert!(a.log.cursor.is_none() && a.log.expanded.is_empty());
        // removed
        std::fs::remove_file(&path).unwrap();
        assert!(a.refresh_log());
        assert!(a.log.lines.is_empty());
        assert!(!a.refresh_log());
    }

    #[test]
    fn refresh_only_applies_in_the_log_view() {
        let (mut a, _fx, _d) = fixture(b"x\n");
        assert!(!a.refresh_log());
    }

    #[test]
    fn non_utf8_and_split_multibyte_lines_decode_lossily() {
        let (mut a, _fx, _d) = open(&[b'a', 0xff, b'b', b'\n', 0xe2, 0x94]);
        assert_eq!(a.log.lines, ["a\u{fffd}b", "\u{fffd}"]);
        let path = a.paths.as_ref().unwrap().log("eink-weather");
        let mut f = std::fs::OpenOptions::new().append(true).open(path).unwrap();
        f.write_all(&[0x80, b'\n']).unwrap(); // completes `─`
        assert!(a.refresh_log());
        assert_eq!(a.log.lines, ["a\u{fffd}b", "─"]);
        text(&mut a);
    }

    #[test]
    fn interior_blank_lines_are_kept_and_the_final_newline_is_not_a_line() {
        let (a, _fx, _d) = open(b"a\n\nb\n");
        assert_eq!(a.log.lines, ["a", "", "b"]);
    }
}
