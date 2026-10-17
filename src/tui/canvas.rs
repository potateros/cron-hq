//! Small drawing helpers shared by the views: styled spans clipped by display width, space-between rows, rules.

use super::theme::{HOLD, MARGIN};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

pub type Span = (String, Style);

pub fn sp(text: impl Into<String>, color: Color) -> Span {
    (text.into(), Style::default().fg(color))
}

pub fn bold(mut s: Span) -> Span {
    s.1 = s.1.add_modifier(Modifier::BOLD);
    s
}

pub fn spans_width(spans: &[Span]) -> usize {
    spans.iter().map(|(t, _)| t.width()).sum()
}

/// Cut `spans` to `w` columns, ending in `…` when anything was removed (Ink's `truncate-end`).
pub fn clip_spans(spans: &[Span], w: usize) -> Vec<Span> {
    if spans_width(spans) <= w {
        return spans.to_vec();
    }
    if w == 0 {
        return Vec::new();
    }
    let keep = w - 1;
    let mut out: Vec<Span> = Vec::new();
    let mut used = 0;
    let mut style = Style::default();
    'outer: for (t, st) in spans {
        style = *st;
        let mut piece = String::new();
        for c in t.chars() {
            let cw = c.width().unwrap_or(0);
            if used + cw > keep {
                if !piece.is_empty() {
                    out.push((piece, *st));
                }
                break 'outer;
            }
            used += cw;
            piece.push(c);
        }
        out.push((piece, *st));
    }
    out.push(("…".to_string(), style));
    out
}

/// Draw `spans` at (x, y), at most `maxw` columns, inside `area`. Returns the width drawn.
pub fn put(buf: &mut Buffer, area: Rect, x: u16, y: u16, maxw: u16, spans: &[Span]) -> u16 {
    if y < area.top() || y >= area.bottom() || x < area.left() || x >= area.right() {
        return 0;
    }
    let maxw = maxw.min(area.right() - x);
    let mut cx = x;
    for (t, st) in clip_spans(spans, maxw as usize) {
        let remaining = maxw - (cx - x);
        let (nx, _) = buf.set_stringn(cx, y, &t, remaining as usize, st);
        cx = nx;
    }
    cx - x
}

/// Left and right text on one row of width `w`; the left side gives way when they collide.
/// Returns the x of the right text.
pub fn between(buf: &mut Buffer, area: Rect, x: u16, y: u16, w: u16, left: &[Span], right: &[Span]) -> u16 {
    let rw = spans_width(right).min(w as usize) as u16;
    let avail = w - rw;
    let lw = spans_width(left).min(w as usize) as u16;
    let lmax = if lw <= avail { avail } else { avail.saturating_sub(1) };
    put(buf, area, x, y, lmax, left);
    let rx = x + w - rw;
    put(buf, area, rx, y, rw, right);
    rx
}

/// The `─` divider, `w` wide, at the content margin.
pub fn rule(buf: &mut Buffer, area: Rect, y: u16, w: u16) {
    put(buf, area, MARGIN, y, w, &[sp("─".repeat(w as usize), HOLD)]);
}

/// The part of `r` that lies inside `area`, if any.
pub fn visible(r: Rect, area: Rect) -> Option<Rect> {
    let i = r.intersection(area);
    (i.width > 0 && i.height > 0).then_some(i)
}
