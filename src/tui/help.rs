//! Per-view help (port of `HelpView.tsx`). Any click closes it, so it registers no hits.

use super::canvas::{between, bold, put, rule, sp};
use super::theme::{inner_width, ACCENT, BODY, FAULT, HOLD, MARGIN};
use super::View;
use ratatui::style::Color;
use ratatui::Frame;
use unicode_width::UnicodeWidthStr;

type Row = (&'static str, &'static str, Option<Color>);

const COL: usize = 12;

fn sections(view: View) -> Vec<(&'static str, Vec<Row>)> {
    match view {
        View::Services => vec![(
            "SERVICES",
            vec![
                ("◉  ○  ◆", "running · stopped or not created · unhealthy, restarting or crashed", Some(ACCENT)),
                ("CPU  MEM", "live usage; CPU is % of one core (can pass 100%); amber over 50% or 1 GiB", None),
                ("O · X · Z", "open localhost:<port> · stop/start one service · stop/start the stack", None),
                ("[stacks.x]", "dir = compose project dir; optional files = [...], env_file", None),
                ("← → · Tab", "switch between the JOBS and SERVICES tabs (or click a tab)", None),
            ],
        )],
        View::Jobs => vec![
            (
                "JOB STATUS",
                vec![
                    ("◉", "nominal — the last run succeeded", Some(ACCENT)),
                    ("◆", "fault — the last run failed (EXIT n) or timed out", Some(FAULT)),
                    ("○", "hold — paused, no scheduled runs", Some(HOLD)),
                    ("⚠", "not synced — launchd differs from jobs.toml; press S", Some(FAULT)),
                    ("RUNNING", "a run is in progress", None),
                    ("T–hh:mm:ss", "countdown to the next scheduled run", None),
                    ("ON DEMAND · R", "manual = true: never scheduled, R runs it", None),
                ],
            ),
            (
                "SUMMARY",
                vec![
                    ("SYSTEMS", "jobs in jobs.toml", None),
                    ("NOMINAL", "last run ok", None),
                    ("FAULT", "last run failed or timed out", None),
                    ("STANDBY", "held, or never run", None),
                ],
            ),
            ("HISTORY", vec![("▁  █  ·", "last 10 runs, newest right: ok · fault · skipped", None)]),
            (
                "DETAILS",
                vec![
                    ("cmd · dir", "command (run with zsh -lc) and working directory; url: O opens it", None),
                    ("quiet", "scheduled runs are skipped in this window (R still runs)", None),
                    ("timeout", "SIGTERM after this long, SIGKILL 10 s later", None),
                    ("launchd", "linked = loaded · held · unlinked = not loaded", None),
                    ("›", "last line of output from the latest run", None),
                ],
            ),
        ],
    }
}

pub fn draw_help(frame: &mut Frame, view: View) {
    let area = frame.area();
    let w = inner_width(area.width);
    let buf = frame.buffer_mut();
    let mut y = 1; // paddingTop
    between(buf, area, MARGIN, y, w, &[bold(sp("◢ H Q · HELP", ACCENT))], &[sp("esc / H close", HOLD)]);
    y += 1;
    rule(buf, area, y, w);
    y += 1;
    for (title, rows) in sections(view) {
        put(buf, area, MARGIN, y, w, &[sp(title, HOLD)]);
        y += 1;
        for (sym, meaning, tone) in rows {
            let pad = " ".repeat(COL.saturating_sub(sym.width()));
            put(buf, area, MARGIN, y, w, &[sp(format!("  {sym}{pad}"), tone.unwrap_or(ACCENT)), sp(meaning, BODY)]);
            y += 1;
        }
    }
    rule(buf, area, y, w);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::testing::lines;

    fn render(view: View, w: u16, h: u16) -> Vec<String> {
        let mut t = ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
        t.draw(|f| draw_help(f, view)).unwrap();
        lines(t.backend().buffer())
    }

    #[test]
    fn jobs_help_explains_the_symbols() {
        let f = render(View::Jobs, 100, 40).join("\n");
        assert!(f.contains("◢ H Q · HELP"));
        assert!(f.contains("esc / H close"));
        for s in ["◉", "◆", "○", "⚠", "T–", "▁", "█", "NOMINAL", "STANDBY", "quiet", "launchd"] {
            assert!(f.contains(s), "{s}");
        }
        assert!(f.contains("  ◉           nominal — the last run succeeded"), "{f}");
    }

    #[test]
    fn services_help_has_its_own_section() {
        let f = render(View::Services, 100, 40).join("\n");
        assert!(f.contains("O · X · Z"));
        assert!(f.contains("[stacks.x]"));
        assert!(!f.contains("JOB STATUS"));
    }
}
