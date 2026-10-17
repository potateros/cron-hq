//! Colours, widths and the small text helpers every view shares (port of `theme.ts`).

use crate::history::Status;
use crate::snapshot::JobView;
use chrono::{DateTime, Local, TimeZone, Timelike};
use ratatui::style::Color;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Dim cyan-grey.
pub const BODY: Color = Color::Rgb(0x7f, 0x9a, 0xa5);
/// Bright cyan.
pub const ACCENT: Color = Color::Rgb(0x5e, 0xe7, 0xff);
/// Amber.
pub const FAULT: Color = Color::Rgb(0xff, 0xb3, 0x47);
/// Grey.
pub const HOLD: Color = Color::Rgb(0x5a, 0x66, 0x70);

pub const MAX_WIDTH: u16 = 90;
/// Left/right margin; everything aligns to the divider between them.
pub const MARGIN: u16 = 2;
/// Indent of a job's second line and of detail fields: lines up under the job name.
pub const INDENT: u16 = 3;

/// Width of the content area (the divider's length) for a terminal this wide.
pub fn inner_width(cols: u16) -> u16 {
    let cols = if cols == 0 { MAX_WIDTH } else { cols };
    cols.min(MAX_WIDTH).saturating_sub(2 * MARGIN)
}

/// The longest prefix of `s` that fits in `width` columns, and the rest.
fn split_width(s: &str, width: usize) -> (&str, &str) {
    let mut w = 0;
    for (i, c) in s.char_indices() {
        let cw = c.width().unwrap_or(0);
        if w + cw > width {
            return (&s[..i], &s[i..]);
        }
        w += cw;
    }
    (s, "")
}

/// Cut `s` to `width` columns, ending in `…` when anything was removed.
pub fn truncate_width(s: &str, width: usize) -> String {
    if s.width() <= width {
        return s.to_string();
    }
    if width == 0 {
        return String::new();
    }
    let (head, _) = split_width(s, width - 1);
    format!("{head}…")
}

/// Word-wrap `text` to `width`, keeping at most `max` lines; an overflowing last line ends with "...".
pub fn clamp_lines(text: &str, width: usize, max: usize) -> Vec<String> {
    if width == 0 {
        return Vec::new();
    }
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    for word in text.split_whitespace() {
        let mut word = word;
        while word.width() > width {
            let room = if cur.is_empty() { width as isize } else { width as isize - cur.width() as isize - 1 };
            if room > 0 {
                let (head, rest) = split_width(word, room as usize);
                if head.is_empty() {
                    if cur.is_empty() {
                        // a glyph wider than the whole line can never fit: drop it
                        word = &rest[rest.chars().next().map_or(0, char::len_utf8)..];
                    } else {
                        lines.push(std::mem::take(&mut cur));
                    }
                    continue;
                }
                lines.push(if cur.is_empty() { head.to_string() } else { format!("{cur} {head}") });
                word = rest;
            } else {
                lines.push(cur.clone());
            }
            cur.clear();
        }
        if cur.is_empty() {
            cur = word.to_string();
        } else if cur.width() + 1 + word.width() <= width {
            cur.push(' ');
            cur.push_str(word);
        } else {
            lines.push(std::mem::take(&mut cur));
            cur = word.to_string();
        }
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    if lines.len() <= max {
        return lines;
    }
    lines.truncate(max);
    if let Some(last) = lines.last_mut() {
        let (head, _) = split_width(last, width.saturating_sub(3));
        *last = format!("{}...", head.trim_end());
    }
    lines
}

pub fn is_fault(v: &JobView) -> bool {
    matches!(v.last.as_ref().map(|r| r.status), Some(Status::Failed | Status::Timeout))
}

pub fn glyph(v: &JobView) -> &'static str {
    if v.held {
        "○"
    } else if !v.synced {
        "⚠"
    } else if is_fault(v) {
        "◆"
    } else {
        "◉"
    }
}

pub fn format_seconds(sec: u64) -> String {
    if sec.is_multiple_of(3600) {
        format!("{}h", sec / 3600)
    } else if sec.is_multiple_of(60) {
        format!("{}m", sec / 60)
    } else {
        format!("{sec}s")
    }
}

/// Local `HH:MM` of an epoch-millisecond timestamp.
pub fn hhmm(ms: i64) -> String {
    match Local.timestamp_millis_opt(ms).single() {
        Some(d) => format!("{:02}:{:02}", d.hour(), d.minute()),
        None => "—".to_string(),
    }
}

pub fn minutes_to_clock(m: u32) -> String {
    format!("{:02}:{:02}", m / 60, m % 60)
}

/// `14:03:09 · SAT 18 OCT`
pub fn clock_line(now: DateTime<Local>) -> String {
    now.format("%H:%M:%S · %a %-d %b").to_string().to_uppercase()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Job, Schedule};
    use crate::history::{RunRow, Trigger};
    use crate::launchd::AgentState;
    use std::path::PathBuf;

    fn view(held: bool, synced: bool, status: Option<Status>) -> JobView {
        JobView {
            job: Job {
                id: "x".into(),
                description: String::new(),
                command: "x".into(),
                cwd: PathBuf::from("/"),
                schedule: Schedule::Manual,
                quiet: None,
                timeout_sec: 60,
                url: None,
            },
            held,
            agent: AgentState { linked: true, running: false, last_exit: None },
            synced,
            last: status.map(|status| RunRow {
                id: 1,
                job: "x".into(),
                trigger: Trigger::Manual,
                started_at: 0,
                ended_at: None,
                exit_code: None,
                status,
                reason: None,
                tail: None,
            }),
            recent: vec![],
            running: false,
            next: None,
        }
    }

    #[test]
    fn clamp_lines_wraps_on_words() {
        assert_eq!(clamp_lines("aa bb cc dd", 5, 5), vec!["aa bb", "cc dd"]);
        assert_eq!(clamp_lines("  one   two ", 20, 2), vec!["one two"]);
        assert!(clamp_lines("", 10, 2).is_empty());
    }

    #[test]
    fn clamp_lines_splits_long_words() {
        assert_eq!(clamp_lines("abcdefghij", 4, 5), vec!["abcd", "efgh", "ij"]);
        assert_eq!(clamp_lines("ab abcdefgh", 6, 5), vec!["ab abc", "defgh"]);
    }

    #[test]
    fn clamp_lines_ends_overflow_with_dots() {
        assert_eq!(clamp_lines("aaaa bbbb cccc dddd eeee", 9, 2), vec!["aaaa bbbb", "cccc d..."]);
        assert_eq!(clamp_lines("alpha beta gamma", 5, 2), vec!["alpha", "be..."]);
    }

    #[test]
    fn clamp_lines_measures_display_width_and_never_panics() {
        for w in 0..8 {
            for max in 1..4 {
                for t in ["😀😀😀😀 ──── 日本語テキスト", "a 😀 b", "──"] {
                    let out = clamp_lines(t, w, max);
                    assert!(out.len() <= max);
                    if w >= 3 {
                        assert!(out.iter().all(|l| l.width() <= w), "{t:?} w={w}: {out:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn truncate_width_counts_display_width() {
        assert_eq!(truncate_width("ab──😀cd", 5), "ab──…");
        assert_eq!(truncate_width("short", 5), "short");
        assert_eq!(truncate_width("abc", 0), "");
        assert_eq!(truncate_width("日本語", 4), "日…");
    }

    #[test]
    fn clock_line_format() {
        let d = Local.with_ymd_and_hms(2026, 10, 18, 14, 3, 9).unwrap();
        assert_eq!(clock_line(d), "14:03:09 · SUN 18 OCT");
        let d = Local.with_ymd_and_hms(2026, 9, 5, 0, 0, 0).unwrap();
        assert_eq!(clock_line(d), "00:00:00 · SAT 5 SEP");
    }

    #[test]
    fn format_seconds_picks_largest_unit() {
        assert_eq!(format_seconds(7200), "2h");
        assert_eq!(format_seconds(300), "5m");
        assert_eq!(format_seconds(90), "90s");
        assert_eq!(format_seconds(45), "45s");
    }

    #[test]
    fn clock_helpers() {
        assert_eq!(minutes_to_clock(0), "00:00");
        assert_eq!(minutes_to_clock(7 * 60 + 5), "07:05");
        let d = Local.with_ymd_and_hms(2026, 10, 18, 9, 7, 30).unwrap();
        assert_eq!(hhmm(d.timestamp_millis()), "09:07");
    }

    #[test]
    fn inner_width_caps_and_defaults() {
        assert_eq!(inner_width(200), 86);
        assert_eq!(inner_width(60), 56);
        assert_eq!(inner_width(0), 86);
        assert_eq!(inner_width(3), 0);
    }

    #[test]
    fn glyphs() {
        assert_eq!(glyph(&view(true, true, None)), "○");
        assert_eq!(glyph(&view(false, false, Some(Status::Ok))), "⚠");
        assert_eq!(glyph(&view(false, true, Some(Status::Failed))), "◆");
        assert_eq!(glyph(&view(false, true, Some(Status::Timeout))), "◆");
        assert_eq!(glyph(&view(false, true, Some(Status::Ok))), "◉");
        assert_eq!(glyph(&view(false, true, None)), "◉");
        assert!(is_fault(&view(false, true, Some(Status::Failed))));
        assert!(!is_fault(&view(false, true, Some(Status::Skipped))));
    }
}
