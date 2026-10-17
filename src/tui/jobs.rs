//! The jobs view (ports of `Dashboard.tsx`, `JobRow.tsx`, `DetailPanel.tsx` and the sync prompt in `App.tsx`).

use super::canvas::{between, bold, put, rule, sp, spans_width, visible, Span};
use super::theme::{
    clamp_lines, clock_line, format_seconds, glyph, hhmm, inner_width, is_fault, minutes_to_clock, ACCENT, BODY, FAULT,
    HOLD, INDENT, MARGIN,
};
use super::{App, Hit, Mode};
use crate::runner::format_duration;
use crate::schedule::{describe_schedule, format_countdown};
use crate::snapshot::{summarize, JobView};
use crossterm::event::KeyCode;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::Frame;
use unicode_width::UnicodeWidthStr;

pub const KEYS: &str = "↑↓ select   ⏎ logs   R run   P hold   S sync   O open   Tab services   H help   Q exit";

/// The key a footer segment stands for; `↑↓` is two keys, so it is not clickable.
fn segment_key(segment: &str) -> Option<KeyCode> {
    let first = segment.split(' ').next()?;
    match first {
        "⏎" => Some(KeyCode::Enter),
        "Tab" => Some(KeyCode::Tab),
        s if s.chars().count() == 1 && s.chars().all(|c| c.is_ascii_alphabetic()) => {
            Some(KeyCode::Char(s.chars().next()?.to_ascii_lowercase()))
        }
        _ => None,
    }
}

fn history_strip(v: &JobView) -> Vec<Span> {
    use crate::history::Status;
    let marks: Vec<(&str, _)> = v
        .recent
        .iter()
        .take(10)
        .rev()
        .map(|r| match r.status {
            Status::Skipped => ("·", HOLD),
            Status::Failed | Status::Timeout => ("█", FAULT),
            _ => ("▁", BODY),
        })
        .collect();
    marks.into_iter().enumerate().map(|(i, (m, c))| sp(format!("{}{m}", if i > 0 { " " } else { "" }), c)).collect()
}

fn exit_text(v: &JobView) -> String {
    let last = v.last.as_ref();
    if last.is_some_and(|r| r.status == crate::history::Status::Timeout) {
        "TIMEOUT".to_string()
    } else {
        // `EXIT null` for a run without an exit code, as the TS version prints it
        format!("EXIT {}", last.and_then(|r| r.exit_code).map_or("null".to_string(), |c| c.to_string()))
    }
}

fn right_status(v: &JobView, now: chrono::DateTime<chrono::Local>) -> Span {
    use crate::config::Schedule;
    if v.held {
        return sp("HOLD", HOLD);
    }
    if v.running {
        return sp("RUNNING", ACCENT);
    }
    if v.job.schedule == Schedule::Manual {
        return if is_fault(v) {
            sp(format!("FAULT · {}   R to retry", exit_text(v)), FAULT)
        } else {
            sp("ON DEMAND · R", BODY)
        };
    }
    let countdown = v.next.map(|n| format_countdown((n - now).num_milliseconds())).unwrap_or_default();
    if !v.synced {
        return sp(format!("⚠ NOT SYNCED   {countdown}").trim_end().to_string(), FAULT);
    }
    if is_fault(v) {
        return sp(format!("FAULT · {}   {countdown}", exit_text(v)).trim_end().to_string(), FAULT);
    }
    sp(countdown, BODY)
}

fn detail_line(v: &JobView) -> String {
    let mut parts = vec![describe_schedule(&v.job.schedule)];
    if v.held {
        parts.push("paused".into());
    } else if let Some(last) = &v.last {
        parts.push(format!("last {}", hhmm(last.started_at)));
        if let Some(end) = last.ended_at {
            parts.push(format_duration(end - last.started_at));
        }
    } else {
        parts.push("never run".into());
    }
    parts.join(" · ")
}

fn field(label: &str, value: String) -> Vec<Span> {
    let pad = " ".repeat(10usize.saturating_sub(label.width()));
    vec![sp(format!("{label}{pad}"), HOLD), sp(value, BODY)]
}

fn pad_end(s: &str, n: usize) -> String {
    format!("{s}{}", " ".repeat(n.saturating_sub(s.width())))
}

type Line = (u16, Vec<Span>);

/// The detail panel as lines: (x, spans).
fn detail_lines(v: &JobView, w: u16) -> Vec<Line> {
    let job = &v.job;
    let mut head = vec![sp(format!("▸  {}", job.id.to_uppercase()), ACCENT)];
    if !job.description.is_empty() {
        head.push(sp(format!(" — {}", job.description), BODY));
    }
    let ix = MARGIN + INDENT;
    let inner = w.saturating_sub(INDENT);
    let quiet = job.quiet.map_or("—".to_string(), |q| format!("{} – {}", minutes_to_clock(q.start.into()), minutes_to_clock(q.end.into())));
    let launchd = if v.held {
        "held"
    } else if v.agent.linked {
        "linked"
    } else {
        "unlinked"
    };
    let mut out: Vec<Line> = vec![(MARGIN, head)];
    out.push((ix, field("cmd", job.command.clone())));
    out.push((ix, field("dir", job.cwd.to_string_lossy().into_owned())));
    if let Some(u) = &job.url {
        out.push((ix, field("url", format!("{u}  (O opens)"))));
    }
    let mut q = field("quiet", pad_end(&quiet, 18));
    q.extend(field("timeout", pad_end(&format_seconds(job.timeout_sec), 8)));
    q.extend(field("launchd", launchd.to_string()));
    out.push((ix, q));
    let last_line = v
        .last
        .as_ref()
        .and_then(|r| r.tail.as_deref())
        .and_then(|t| t.split('\n').rfind(|l| !l.trim().is_empty()))
        .unwrap_or("no output yet");
    for (i, l) in clamp_lines(last_line, (inner as usize).saturating_sub(2), 2).iter().enumerate() {
        out.push((ix, vec![sp(format!("{}{l}", if i > 0 { "  " } else { "› " }), BODY)]));
    }
    out
}

/// Draw `lines` from row `y`; the first line (the panel title) may use the full width, the rest the indented width.
fn draw_detail(buf: &mut ratatui::buffer::Buffer, area: Rect, mut y: u16, w: u16, lines: &[Line]) {
    for (x, spans) in lines {
        put(buf, area, *x, y, w.saturating_sub(x - MARGIN), spans);
        y += 1;
    }
}

/// The sync prompt's text lines and whether the last one carries `Y confirm`.
fn prompt_lines(res: &Result<crate::launchd::SyncPlan, String>) -> (Vec<(String, ratatui::style::Color)>, bool) {
    let mut lines = Vec::new();
    match res {
        Err(e) => {
            lines.push((e.clone(), FAULT));
            lines.push(("any key to close".to_string(), HOLD));
            (lines, false)
        }
        Ok(plan) => {
            lines.extend(plan.add.iter().map(|id| (format!("+ {id}"), ACCENT)));
            lines.extend(plan.change.iter().map(|id| (format!("~ {id}"), BODY)));
            lines.extend(plan.remove.iter().map(|id| (format!("- {id}"), FAULT)));
            if plan.add.is_empty() && plan.change.is_empty() && plan.remove.is_empty() {
                lines.push(("in sync · any key to close".to_string(), HOLD));
                (lines, false)
            } else {
                lines.push(("apply sync?  Y confirm · any key cancel".to_string(), HOLD));
                (lines, true)
            }
        }
    }
}

/// Draw prompt lines from `y`, registering the `Y confirm` target on the last one.
fn draw_sync_prompt(
    buf: &mut ratatui::buffer::Buffer,
    area: Rect,
    mut y: u16,
    lines: &[(String, ratatui::style::Color)],
    confirm: bool,
    hits: &mut Vec<(Rect, Hit)>,
) {
    for (i, (t, c)) in lines.iter().enumerate() {
        put(buf, area, MARGIN, y, area.width.saturating_sub(MARGIN), &[sp(t.clone(), *c)]);
        if confirm && i + 1 == lines.len() {
            let prefix = t.split("Y confirm").next().unwrap_or("").width() as u16;
            if let Some(r) = visible(Rect::new(MARGIN + prefix, y, "Y confirm".width() as u16, 1), area) {
                hits.push((r, Hit::Confirm));
            }
        }
        y += 1;
    }
}

/// The footer; each `<key> <label>` segment is a click target.
fn draw_footer(buf: &mut ratatui::buffer::Buffer, area: Rect, y: u16, w: u16, hits: &mut Vec<(Rect, Hit)>) {
    put(buf, area, MARGIN, y, w, &[sp(KEYS, HOLD)]);
    let mut x = MARGIN;
    for seg in KEYS.split("   ") {
        let sw = seg.width() as u16;
        if let Some(code) = segment_key(seg) {
            let r = Rect::new(x, y, sw.min((MARGIN + w).saturating_sub(x)), 1);
            if let Some(r) = visible(r, area) {
                hits.push((r, Hit::Key(code)));
            }
        }
        x += sw + 3;
    }
}

/// `HQ · JOBS`: draw the whole view and register its clickable regions.
///
/// Everything above and below the job list is fixed; the sync prompt replaces the detail panel, and the job
/// rows are windowed around the selection so the footer always keeps the last row.
pub fn draw_jobs(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    let w = inner_width(area.width);
    let mut hits: Vec<(Rect, Hit)> = Vec::new();
    let banner = app.history_banner();
    let docker = app.docker_line();
    let now = app.now;
    let buf = frame.buffer_mut();
    let mut y: u16 = 0;

    let Some(snap) = &app.snap else {
        put(buf, area, 0, y, area.width, &[sp("   ◢ H Q  ·  linking…", BODY)]);
        if let Some(b) = &banner {
            put(buf, area, 0, y + 1, area.width, &[sp(format!("  {b}"), FAULT)]);
        }
        app.hits = hits;
        return;
    };
    if let Some(b) = &banner {
        put(buf, area, 0, y, area.width, &[sp(format!("  {b}"), FAULT)]);
        y += 1;
    }
    let count = snap.jobs.len();
    app.selected = app.selected.min(count.saturating_sub(1));
    let selected = app.selected;
    y += 1; // paddingTop

    between(buf, area, MARGIN, y, w, &[bold(sp("◢ H Q", ACCENT))], &[sp(clock_line(now), BODY)]);
    y += 1;
    if let Some(e) = &snap.config_error {
        put(buf, area, MARGIN, y, w, &[sp(format!("CONFIG ERROR — {e}"), FAULT)]);
        y += 1;
    }
    rule(buf, area, y, w);
    y += 1;

    // SYSTEMS / NOMINAL / FAULT / STANDBY spread across the divider's width.
    let sum = summarize(snap);
    let items = [
        (format!("SYSTEMS {:02}", sum.systems), BODY),
        (format!("NOMINAL {:02}", sum.nominal), BODY),
        (format!("FAULT {:02}", sum.fault), if sum.fault > 0 { FAULT } else { BODY }),
        (format!("STANDBY {:02}", sum.standby), BODY),
    ];
    let total: usize = items.iter().map(|(t, _)| t.width()).sum();
    let free = (w as usize).saturating_sub(total);
    let mut before = 0;
    for (i, (t, c)) in items.iter().enumerate() {
        let x = MARGIN + (before + free * i / 3) as u16;
        put(buf, area, x, y, w.saturating_sub(x - MARGIN), &[sp(t.clone(), *c)]);
        before += t.width();
    }
    y += 1;

    if let Some(d) = &docker {
        let hint = [sp("Tab ▸", HOLD)];
        let rx = between(buf, area, MARGIN, y, w, &[sp(format!("SERVICES {d}"), HOLD)], &hint);
        if let Some(r) = visible(Rect::new(rx, y, spans_width(&hint) as u16, 1), area) {
            hits.push((r, Hit::ToggleView));
        }
        y += 1;
    }
    rule(buf, area, y, w);
    y += 1;

    // The region under the job list: the sync prompt when open, else the selected job's details.
    let prompt = match &app.mode {
        Mode::Sync(res) => Some(prompt_lines(res)),
        _ => None,
    };
    let detail = match (&prompt, snap.jobs.get(selected)) {
        (None, Some(v)) => detail_lines(v, w),
        (None, None) => vec![(MARGIN, vec![sp("no jobs in jobs.toml", HOLD)])],
        _ => Vec::new(),
    };
    let lead_rule = prompt.is_some() || !snap.jobs.is_empty();
    // Rows left for the region and the jobs: all but the closing rule and the footer.
    let rest = (area.height as usize).saturating_sub(y as usize + 2 + usize::from(lead_rule));
    let wanted = prompt.as_ref().map_or(detail.len(), |(l, _)| l.len());
    let region = wanted.min(rest.saturating_sub(2).max(1)); // keep room for at least one job row
    let k = (rest.saturating_sub(region) / 2).clamp(1, count.max(1));
    // When something had to give, leftover rows go to the region so the footer keeps the last row.
    let squeezed = k < count || region < wanted;
    let slack = if squeezed { rest.saturating_sub(region + 2 * k) } else { 0 };
    let start = if count > k { selected.saturating_sub(k / 2).min(count - k) } else { 0 };

    for (i, v) in snap.jobs.iter().enumerate().skip(start).take(k) {
        let sel = i == selected;
        let g = glyph(v);
        let gc = match g {
            "◆" | "⚠" => FAULT,
            "○" => HOLD,
            _ if sel => ACCENT,
            _ => BODY,
        };
        let name_style = Style::default().fg(if sel { ACCENT } else { BODY });
        let name = (format!("  {}", v.job.id.to_uppercase()), if sel { name_style.add_modifier(Modifier::BOLD) } else { name_style });
        between(buf, area, MARGIN, y, w, &[sp(g, gc), name], &[right_status(v, now)]);
        let mut detail_style = Style::default().fg(BODY);
        if !sel {
            detail_style = detail_style.add_modifier(Modifier::DIM);
        }
        let line = (format!("{}{}", " ".repeat(INDENT as usize), detail_line(v)), detail_style);
        between(buf, area, MARGIN, y + 1, w, &[line], &history_strip(v));
        if let Some(r) = visible(Rect::new(MARGIN, y, w, 2), area) {
            hits.push((r, Hit::Job(i)));
        }
        y += 2;
    }

    if lead_rule {
        rule(buf, area, y, w);
        y += 1;
    }
    match &prompt {
        Some((lines, confirm)) => {
            let from = lines.len() - region.min(lines.len()); // the last lines: the question stays visible
            draw_sync_prompt(buf, area, y, &lines[from..], *confirm, &mut hits);
        }
        None => draw_detail(buf, area, y, w, &detail[..region.min(detail.len())]),
    }
    y += (region + slack) as u16;
    rule(buf, area, y, w);
    draw_footer(buf, area, y + 1, w, &mut hits);
    app.hits = hits;
}

impl App {
    fn current_job(&self) -> Option<&JobView> {
        let jobs = &self.snap.as_ref()?.jobs;
        jobs.get(self.selected.min(jobs.len().saturating_sub(1)))
    }

    /// Jobs dash keys: arrows, Enter, R, P, O, S.
    pub(super) fn jobs_key(&mut self, ev: crossterm::event::KeyEvent, fx: &dyn super::Effects) {
        let count = self.snap.as_ref().map_or(0, |s| s.jobs.len());
        let cur = self.selected.min(count.saturating_sub(1));
        match ev.code {
            KeyCode::Up => self.selected = cur.saturating_sub(1),
            KeyCode::Down => self.selected = (cur + 1).min(count.saturating_sub(1)),
            KeyCode::Enter => {
                if let Some(v) = self.current_job() {
                    self.mode = Mode::Log(v.job.id.clone());
                }
            }
            KeyCode::Char(c) => match c.to_ascii_lowercase() {
                'r' => {
                    if let Some(id) = self.current_job().map(|v| v.job.id.clone()) {
                        fx.spawn_cli(&["run", &id, "--manual"]);
                        self.refresh.agents = true;
                        self.mode = Mode::Log(id);
                    }
                }
                'p' => {
                    if let Some((id, held)) = self.current_job().map(|v| (v.job.id.clone(), v.held)) {
                        fx.spawn_cli(&[if held { "resume" } else { "hold" }, &id]);
                        self.refresh.agents = true;
                    }
                }
                'o' => {
                    if let Some(url) = self.current_job().and_then(|v| v.job.url.clone()) {
                        fx.open(&url);
                    }
                }
                's' => self.mode = Mode::Sync(self.plan_sync_now()),
                _ => {}
            },
            _ => {}
        }
    }

    fn plan_sync_now(&self) -> Result<crate::launchd::SyncPlan, String> {
        let paths = self.paths.as_ref().ok_or_else(|| "ERROR — no paths".to_string())?;
        let jobs = crate::config::load_config(&paths.config).map_err(|e| format!("CONFIG ERROR — {}", e.0))?;
        crate::launchd::plan_sync(&jobs, &crate::launchd::PlistCtx::current(paths), paths).map_err(|e| format!("ERROR — {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::launchd::SyncPlan;
    use crate::stacks::{ServiceState, ServiceView, Stack, StackView};
    use crate::tui::testing::{find, job, lines, now, snapshot, views, Rec};
    use crate::tui::{draw, View};
    use crossterm::event::{KeyEvent, KeyEventKind, KeyEventState, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

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

    fn app_with(snap: crate::snapshot::Snapshot) -> App {
        let mut a = App::new(now());
        a.snap = Some(snap);
        a
    }
    fn render_size(a: &mut App, w: u16, h: u16) -> Vec<String> {
        let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
        t.draw(|f| draw(f, a)).unwrap();
        lines(t.backend().buffer())
    }
    fn render(a: &mut App) -> Vec<String> {
        render_size(a, 100, 40)
    }
    fn text(a: &mut App) -> String {
        render(a).join("\n")
    }
    fn row_of(ls: &[String], id: &str) -> String {
        ls.iter().find(|l| l.contains(id)).unwrap().clone()
    }

    #[test]
    fn title_and_summary() {
        let f = text(&mut app_with(snapshot()));
        for s in ["◢ H Q", "17:32:08 · FRI 16 OCT", "SYSTEMS 04", "NOMINAL 02", "FAULT 01", "STANDBY 01"] {
            assert!(f.contains(s), "{s}\n{f}");
        }
    }

    #[test]
    fn job_rows() {
        let ls = render(&mut app_with(snapshot()));
        assert!(row_of(&ls, "EINK-WEATHER").contains("◉"));
        assert!(row_of(&ls, "EINK-WEATHER").contains("T–00:27:52"));
        assert!(row_of(&ls, "TAILSCALE-WATCHDOG").contains("◆"));
        assert!(row_of(&ls, "TAILSCALE-WATCHDOG").contains("FAULT · EXIT 1"));
        assert!(row_of(&ls, "EINK-WIPE").contains("○"));
        assert!(row_of(&ls, "EINK-WIPE").contains("HOLD"));
        assert!(row_of(&ls, "TRML-REMINDERS").contains("⚠ NOT SYNCED"));
    }

    #[test]
    fn second_line_schedule_last_run_duration_and_strip() {
        let ls = render(&mut app_with(snapshot()));
        let i = ls.iter().position(|l| l.contains("◉  EINK-WEATHER")).unwrap();
        assert!(ls[i + 1].contains("every 30m · last 17:30 · 41s"), "{}", ls[i + 1]);
        assert!(ls[i + 1].contains("· █ ▁"), "oldest to newest: skipped, failed, ok");
        let w = ls.iter().position(|l| l.contains("EINK-WIPE")).unwrap();
        assert!(ls[w + 1].contains("daily 06:55 · paused"));
    }

    #[test]
    fn detail_panel_for_the_selected_job() {
        let mut a = app_with(snapshot());
        let f = text(&mut a);
        for s in [
            "▸  EINK-WEATHER — Punggol weather → nRF e-ink",
            "cmd       uv run python -m epd.cli weather --push",
            "dir       /Users/x/Developer/potateros/nrf5-eink",
            "quiet     23:00 – 07:00",
            "timeout   5m",
            "launchd   linked",
            "› pushed (battery 2921 mV, clock drift -3s), fallback armed for midnight",
        ] {
            assert!(f.contains(s), "{s}\n{f}");
        }
        a.selected = 1;
        assert!(text(&mut a).contains("▸  TAILSCALE-WATCHDOG"));
    }

    #[test]
    fn footer_keys() {
        let f = text(&mut app_with(snapshot()));
        assert!(f.contains(KEYS));
        assert_eq!(KEYS, "↑↓ select   ⏎ logs   R run   P hold   S sync   O open   Tab services   H help   Q exit");
    }

    #[test]
    fn config_error_banner_keeps_the_job_list() {
        let mut s = snapshot();
        s.config_error = Some("jobs.eink: unknown key colour".into());
        let f = text(&mut app_with(s));
        assert!(f.contains("CONFIG ERROR — jobs.eink: unknown key colour"));
        assert!(f.contains("EINK-WEATHER"));
    }

    fn long_snap() -> crate::snapshot::Snapshot {
        let mut v = views();
        v[0].job.command = format!("uv run {}", "--flag ".repeat(30));
        let tail = format!("Traceback: {}", "something went quite wrong in the push ".repeat(8));
        v[0].last.as_mut().unwrap().tail = Some(tail);
        crate::snapshot::Snapshot { jobs: v, ..snapshot() }
    }

    #[test]
    fn summary_row_spans_the_divider_exactly() {
        let ls = render(&mut app_with(snapshot()));
        let div = ls.iter().find(|l| l.contains("────")).unwrap();
        let sum = ls.iter().find(|l| l.contains("SYSTEMS")).unwrap();
        assert_eq!(sum.find("SYSTEMS"), div.find('─'));
        assert_eq!(sum.trim_end().width(), div.trim_end().width());
    }

    #[test]
    fn no_line_is_wider_than_the_divider() {
        let ls = render(&mut app_with(long_snap()));
        let right = ls.iter().find(|l| l.contains("────")).unwrap().trim_end().width();
        for l in &ls {
            assert!(l.trim_end().width() <= right, "{l:?}");
        }
    }

    #[test]
    fn long_latest_output_wraps_to_two_lines_and_ends_with_dots() {
        let ls = render(&mut app_with(long_snap()));
        let i = ls.iter().position(|l| l.contains("› Traceback:")).unwrap();
        assert!(ls[i + 1].trim_end().ends_with("..."), "{}", ls[i + 1]);
        assert!(ls[i + 2].contains("────"));
    }

    #[test]
    fn compact_four_jobs_fit_80x24_with_no_blank_rows_between() {
        let ls = render_size(&mut app_with(snapshot()), 80, 24);
        let used = ls.iter().rposition(|l| !l.trim().is_empty()).unwrap() + 1;
        assert!(used <= 24);
        let a = ls.iter().position(|l| l.contains("EINK-WEATHER")).unwrap();
        let b = ls.iter().position(|l| l.contains("TRML-REMINDERS")).unwrap();
        assert!(ls[a..=b + 1].iter().all(|l| !l.trim().is_empty()));
    }

    #[test]
    fn config_error_and_history_banner_and_linking() {
        let mut a = App::new(now());
        a.history_error = Some("locked".into());
        let f = text(&mut a);
        assert!(f.contains("◢ H Q  ·  linking…") && f.contains("ERROR — locked"), "{f}");
        a.snap = Some(snapshot());
        let ls = render(&mut a);
        assert_eq!(ls[0].trim_end(), "  ERROR — locked");
        assert!(ls[2].contains("◢ H Q"));
    }

    fn with_services_app() -> App {
        with_services()
    }

    fn with_services() -> App {
        let mut a = app_with(snapshot());
        let svc = |name: &str, state, cpu, mem| ServiceView {
            stack: "media".into(),
            service: name.into(),
            container: Some(name.into()),
            image: "img".into(),
            ports: vec![],
            state,
            health: String::new(),
            status: String::new(),
            exit_code: 0,
            cpu: Some(cpu),
            mem: Some(mem),
        };
        let stack = Stack { name: "media".into(), dir: "/".into(), files: vec![], env_file: None };
        a.stacks = Some(vec![StackView {
            stack,
            services: vec![svc("a", ServiceState::Running, 0.6, 499 << 20), svc("b", ServiceState::Exited, 0.0, 0)],
            error: None,
        }]);
        a
    }

    #[test]
    fn services_summary_line_only_when_stacks_are_configured() {
        assert!(!text(&mut app_with(snapshot())).contains("SERVICES"));
        let mut a = with_services();
        let ls = render(&mut a);
        let row = row_of(&ls, "SERVICES");
        assert!(row.contains("SERVICES 1/2 up · cpu 0.6% · mem 499M"), "{row}");
        assert!(row.trim_end().ends_with("Tab ▸"));
    }

    #[test]
    fn keys_move_open_run_hold() {
        let (mut a, fx) = (app_with(snapshot()), Rec::default());
        a.handle_key(k(KeyCode::Down), &fx);
        assert_eq!(a.selected, 1);
        for _ in 0..9 {
            a.handle_key(k(KeyCode::Down), &fx);
        }
        assert_eq!(a.selected, 3, "stops at the last job");
        a.handle_key(k(KeyCode::Up), &fx);
        a.selected = 0;
        a.handle_key(k(KeyCode::Up), &fx);
        assert_eq!(a.selected, 0);
        a.handle_key(k(KeyCode::Enter), &fx);
        assert!(matches!(&a.mode, Mode::Log(id) if id == "eink-weather"));
        a.mode = Mode::Dash;
        a.handle_key(k(KeyCode::Char('R')), &fx);
        assert_eq!(*fx.cli.borrow(), vec![vec!["run", "eink-weather", "--manual"]]);
        assert!(matches!(&a.mode, Mode::Log(id) if id == "eink-weather"));
        a.mode = Mode::Dash;
        a.handle_key(k(KeyCode::Char('p')), &fx);
        assert_eq!(fx.cli.borrow().last().unwrap(), &vec!["hold", "eink-weather"]);
        a.selected = 2; // eink-wipe is held
        a.handle_key(k(KeyCode::Char('p')), &fx);
        assert_eq!(fx.cli.borrow().last().unwrap(), &vec!["resume", "eink-wipe"]);
        assert!(a.refresh.agents);
    }

    #[test]
    fn o_opens_the_url_only_when_set() {
        let (mut a, fx) = (app_with(snapshot()), Rec::default());
        a.handle_key(k(KeyCode::Char('o')), &fx);
        assert!(fx.opened.borrow().is_empty());
        a.snap.as_mut().unwrap().jobs[0].job.url = Some("http://localhost:1".into());
        assert!(text(&mut a).contains("url       http://localhost:1  (O opens)"));
        a.handle_key(k(KeyCode::Char('o')), &fx);
        assert_eq!(*fx.opened.borrow(), vec!["http://localhost:1"]);
    }

    #[test]
    fn keys_do_nothing_without_jobs() {
        let (mut a, fx) = (app_with(crate::snapshot::Snapshot { jobs: vec![], ..snapshot() }), Rec::default());
        for c in [KeyCode::Down, KeyCode::Enter, KeyCode::Char('r'), KeyCode::Char('p'), KeyCode::Char('o')] {
            assert!(a.handle_key(k(c), &fx));
        }
        assert!(matches!(a.mode, Mode::Dash) && fx.cli.borrow().is_empty());
        assert!(text(&mut a).contains("no jobs in jobs.toml"));
    }

    fn synced_app() -> (App, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let paths = crate::paths::Paths::temp(dir.path());
        std::fs::write(&paths.config, "[jobs.alpha]\ncommand = \"true\"\nevery = \"5m\"\n\n[jobs.beta]\ncommand = \"true\"\nevery = \"1h\"\n").unwrap();
        let mut a = app_with(snapshot());
        a.paths = Some(paths);
        (a, dir)
    }

    #[test]
    fn s_shows_the_plan_y_applies_anything_else_cancels() {
        let (mut a, _d) = synced_app();
        let fx = Rec::default();
        a.handle_key(k(KeyCode::Char('s')), &fx);
        let f = text(&mut a);
        assert!(f.contains("+ alpha") && f.contains("+ beta"), "{f}");
        assert!(f.contains("apply sync?  Y confirm · any key cancel"));
        a.handle_key(k(KeyCode::Char('n')), &fx);
        assert!(fx.cli.borrow().is_empty());
        assert!(text(&mut a).contains("SYSTEMS"));
        a.handle_key(k(KeyCode::Char('s')), &fx);
        a.handle_key(k(KeyCode::Char('y')), &fx);
        assert_eq!(*fx.cli.borrow(), vec![vec!["sync", "--yes"]]);
        assert!(matches!(a.mode, Mode::Dash));
    }

    #[test]
    fn sync_prompt_variants() {
        let (mut a, _d) = synced_app();
        a.mode = Mode::Sync(Ok(SyncPlan { add: vec!["a".into()], change: vec!["c".into()], remove: vec!["r".into()] }));
        let ls: Vec<String> = render(&mut a).iter().map(|l| l.trim_end().to_string()).collect();
        for want in ["  + a", "  ~ c", "  - r"] {
            assert!(ls.iter().any(|l| l == want), "{want}: {ls:?}");
        }
        a.mode = Mode::Sync(Ok(SyncPlan { add: vec![], change: vec![], remove: vec![] }));
        assert!(text(&mut a).contains("in sync · any key to close"));
        assert!(a.hits.iter().all(|(_, h)| *h != Hit::Confirm), "nothing to confirm");
        a.mode = Mode::Sync(Err("CONFIG ERROR — bad".into()));
        let f = text(&mut a);
        assert!(f.contains("CONFIG ERROR — bad") && f.contains("any key to close"));
    }

    #[test]
    fn s_with_a_broken_config_shows_config_error() {
        let (mut a, _d) = synced_app();
        let p = a.paths.clone().unwrap();
        std::fs::write(&p.config, "[jobs.alpha]\ncommand = ").unwrap();
        a.handle_key(k(KeyCode::Char('s')), &Rec::default());
        assert!(text(&mut a).contains("CONFIG ERROR — "));
    }

    #[test]
    fn s_with_an_unreadable_agents_dir_shows_the_io_error() {
        let (mut a, _d) = synced_app();
        let p = a.paths.clone().unwrap();
        std::fs::write(&p.agents, "not a dir").unwrap();
        a.handle_key(k(KeyCode::Char('s')), &Rec::default());
        let f = text(&mut a);
        assert!(f.contains("ERROR — "), "{f}");
        assert!(!f.contains("CONFIG ERROR"), "{f}");
        assert!(f.contains("Not a directory"), "the io error text is shown: {f}");
        assert!(f.contains("any key to close"));
    }

    fn five_jobs() -> crate::snapshot::Snapshot {
        let mut s = snapshot();
        s.jobs.push(crate::snapshot::JobView { job: job("fifth", crate::config::Schedule::Manual), ..views().remove(0) });
        s
    }

    #[test]
    fn sync_prompt_fits_80x24_and_confirm_applies() {
        for with_services in [false, true] {
            let (mut a, _d) = synced_app();
            a.snap = Some(five_jobs());
            if with_services {
                a.stacks = with_services_app().stacks;
            }
            let fx = Rec::default();
            a.handle_key(k(KeyCode::Char('s')), &fx);
            let ls = render_size(&mut a, 80, 24);
            let f = ls.join("\n");
            assert!(f.contains("+ alpha") && f.contains("+ beta"), "{f}");
            assert!(f.contains("R run"), "footer stays: {f}");
            let (c, r) = find(&ls, "Y confirm");
            assert!(a.hits.iter().any(|(rc, h)| *h == Hit::Confirm && rc.y == r));
            a.handle_mouse(click(c, r), &fx);
            assert_eq!(*fx.cli.borrow(), vec![vec!["sync", "--yes"]], "services line: {with_services}");
        }
    }

    #[test]
    fn long_prompt_keeps_the_question_visible() {
        let (mut a, _d) = synced_app();
        a.snap = Some(five_jobs());
        let ids: Vec<String> = (0..40).map(|i| format!("job{i}")).collect();
        a.mode = Mode::Sync(Ok(SyncPlan { add: ids, change: vec![], remove: vec![] }));
        let ls = render_size(&mut a, 80, 24);
        assert!(ls.iter().any(|l| l.contains("apply sync?  Y confirm")));
        assert!(ls[23].contains("R run"), "{:?}", ls[23]);
        assert!(a.hits.iter().any(|(_, h)| *h == Hit::Confirm));
    }

    #[test]
    fn many_jobs_window_around_the_selection() {
        let mut s = snapshot();
        let mut extra = Vec::new();
        for i in 0..8 {
            extra.push(crate::snapshot::JobView {
                job: job(&format!("extra-{i:02}"), crate::config::Schedule::Manual),
                ..views().remove(0)
            });
        }
        s.jobs.extend(extra);
        assert_eq!(s.jobs.len(), 12);
        let mut a = app_with(s);
        a.selected = 11;
        let ls = render_size(&mut a, 80, 24);
        assert!(ls.iter().any(|l| l.contains("EXTRA-07")), "selected row visible");
        assert!(ls[23].contains("R run"), "footer on the last row: {:?}", ls[23]);
        assert!(ls.iter().any(|l| l.contains("▸  EXTRA-07")), "detail kept");
        let (c, r) = find(&ls, "EXTRA-07");
        assert!(a.hits.iter().any(|(rc, h)| *h == Hit::Job(11) && rc.y <= r && r < rc.y + rc.height));
        let _ = c;
        a.selected = 0;
        let ls = render_size(&mut a, 80, 24);
        assert!(ls.iter().any(|l| l.contains("EINK-WEATHER")));
        assert!(ls[23].contains("R run"));
    }

    #[test]
    fn history_error_renders_as_a_banner() {
        let mut a = app_with(snapshot());
        a.history_error = Some("launchctl went away".into());
        assert!(text(&mut a).contains("ERROR — launchctl went away"));
    }

    #[test]
    fn h_opens_help_esc_or_h_returns() {
        let (mut a, fx) = (app_with(snapshot()), Rec::default());
        a.handle_key(k(KeyCode::Char('h')), &fx);
        let f = text(&mut a);
        assert!(f.contains("HELP"));
        a.handle_key(k(KeyCode::Esc), &fx);
        assert!(text(&mut a).contains("SYSTEMS"));
        a.handle_key(k(KeyCode::Char('h')), &fx);
        a.handle_key(k(KeyCode::Char('h')), &fx);
        assert!(text(&mut a).contains("SYSTEMS"));
        assert!(!a.handle_key(k(KeyCode::Char('q')), &fx));
    }

    #[test]
    fn renders_tiny_terminal_without_panic() {
        let mut a = with_services();
        a.history_error = Some("x".repeat(50));
        a.snap.as_mut().unwrap().config_error = Some("😀".repeat(30));
        a.snap.as_mut().unwrap().jobs[0].job.url = Some("http://example.com".into());
        for (w, h) in [(20, 5), (1, 1), (3, 2), (0, 0)] {
            for (mode, view) in [
                (Mode::Dash, View::Jobs),
                (Mode::Help, View::Jobs),
                (Mode::Help, View::Services),
                (Mode::Log("eink-weather".into()), View::Jobs),
                (Mode::Dash, View::Services),
                (Mode::Sync(Ok(SyncPlan { add: vec!["alpha".into()], change: vec![], remove: vec![] })), View::Jobs),
                (Mode::Sync(Err("e".into())), View::Jobs),
            ] {
                a.mode = mode;
                a.view = view;
                render_size(&mut a, w, h);
            }
        }
        a.snap = None;
        a.mode = Mode::Dash;
        a.view = View::Jobs;
        render_size(&mut a, 20, 5);
    }

    #[test]
    fn row_with_emoji_description_fits_width() {
        let mut s = snapshot();
        s.jobs[0].job.description = "😀 日本語のとても長い説明 🚀 ".repeat(8);
        s.jobs[0].job.command = "echo 😀日本語 ".repeat(20);
        s.jobs[0].last.as_mut().unwrap().tail = Some("🚀 出力 ".repeat(40));
        s.jobs.push(crate::snapshot::JobView {
            job: job("😀-job-with-a-very-long-日本語-name-that-keeps-going-and-going-and-going-on-and-on-and-on", crate::config::Schedule::Manual),
            ..views().remove(0)
        });
        s.config_error = Some("😀".repeat(80));
        let mut a = app_with(s);
        for sel in [0, 4] {
            a.selected = sel;
            let max = (inner_width(100) + MARGIN) as usize;
            for l in render(&mut a) {
                assert!(l.trim_end().width() <= max, "{} > {max}: {l:?}", l.trim_end().width());
            }
        }
    }

    #[test]
    fn click_selects_job_then_opens_log() {
        let (mut a, fx) = (app_with(snapshot()), Rec::default());
        let ls = render(&mut a);
        let (c, r) = find(&ls, "TAILSCALE-WATCHDOG");
        a.handle_mouse(click(c, r), &fx);
        assert_eq!(a.selected, 1);
        assert!(matches!(a.mode, Mode::Dash));
        render(&mut a);
        // the second line of the row is part of the same target
        let ls = render(&mut a);
        let (c, r) = find(&ls, "every 1h");
        a.handle_mouse(click(c, r), &fx);
        assert!(matches!(&a.mode, Mode::Log(id) if id == "tailscale-watchdog"));
        // a click on a rule line or the footer text outside any segment does nothing to the selection
        a.mode = Mode::Dash;
        let ls = render(&mut a);
        let (c, r) = find(&ls, "────");
        a.handle_mouse(click(c, r), &fx);
        assert_eq!(a.selected, 1);
    }

    #[test]
    fn wheel_moves_selection() {
        let (mut a, fx) = (app_with(snapshot()), Rec::default());
        render(&mut a);
        a.handle_mouse(wheel(false), &fx);
        assert_eq!(a.selected, 1);
        a.handle_mouse(wheel(false), &fx);
        a.handle_mouse(wheel(true), &fx);
        assert_eq!(a.selected, 1);
    }

    #[test]
    fn click_footer_key_runs_it() {
        let (mut a, fx) = (app_with(snapshot()), Rec::default());
        let ls = render(&mut a);
        let (c, r) = find(&ls, "R run");
        a.handle_mouse(click(c, r), &fx);
        assert_eq!(*fx.cli.borrow(), vec![vec!["run", "eink-weather", "--manual"]]);
        assert!(matches!(a.mode, Mode::Log(_)));
        a.mode = Mode::Dash;
        let (c, r) = find(&ls, "H help");
        a.handle_mouse(click(c + 5, r), &fx);
        assert!(matches!(a.mode, Mode::Help), "the whole segment is clickable");
        a.mode = Mode::Dash;
        let (c, r) = find(&ls, "↑↓ select");
        a.handle_mouse(click(c, r), &fx);
        assert!(matches!(a.mode, Mode::Dash) && a.selected == 0, "arrows are not a single key");
        let (c, r) = find(&ls, "⏎ logs");
        a.handle_mouse(click(c, r), &fx);
        assert!(matches!(a.mode, Mode::Log(_)));
        a.mode = Mode::Dash;
        let (c, r) = find(&ls, "Tab services");
        a.handle_mouse(click(c, r), &fx);
        assert_eq!(a.view, View::Services);
        a.view = View::Jobs;
        let (c, r) = find(&ls, "Q exit");
        assert!(!a.handle_mouse(click(c, r), &fx));
    }

    #[test]
    fn click_tab_hint_switches_view() {
        let (mut a, fx) = (with_services(), Rec::default());
        let ls = render(&mut a);
        let row = row_of(&ls, "SERVICES");
        let (c, r) = find(std::slice::from_ref(&row), "Tab ▸");
        let r = ls.iter().position(|l| *l == row).unwrap() as u16 + r;
        a.handle_mouse(click(c, r), &fx);
        assert_eq!(a.view, View::Services);
        assert!(a.refresh.stacks);
    }

    #[test]
    fn sync_prompt_click_confirm_vs_elsewhere() {
        let (mut a, _d) = synced_app();
        let fx = Rec::default();
        a.handle_key(k(KeyCode::Char('s')), &fx);
        let ls = render(&mut a);
        let (c, r) = find(&ls, "any key cancel");
        a.handle_mouse(click(c, r), &fx);
        assert!(matches!(a.mode, Mode::Dash) && fx.cli.borrow().is_empty(), "text beside the segment cancels");
        a.handle_key(k(KeyCode::Char('s')), &fx);
        let ls = render(&mut a);
        let (c, r) = find(&ls, "TAILSCALE-WATCHDOG");
        a.handle_mouse(click(c, r), &fx);
        assert!(matches!(a.mode, Mode::Dash) && fx.cli.borrow().is_empty());
        assert_eq!(a.selected, 0, "does not fall through to the row");
        a.handle_key(k(KeyCode::Char('s')), &fx);
        let ls = render(&mut a);
        let (c, r) = find(&ls, "Y confirm");
        a.handle_mouse(click(c + 3, r), &fx);
        assert_eq!(*fx.cli.borrow(), vec![vec!["sync", "--yes"]]);
        assert!(matches!(a.mode, Mode::Dash));
    }

    #[test]
    fn click_closes_help() {
        let (mut a, fx) = (app_with(snapshot()), Rec::default());
        a.mode = Mode::Help;
        let ls = render(&mut a);
        assert!(a.hits.is_empty());
        let (c, r) = find(&ls, "nominal");
        a.handle_mouse(click(c, r), &fx);
        assert!(matches!(a.mode, Mode::Dash));
    }

    #[test]
    fn hits_are_clipped_to_the_screen() {
        let mut a = app_with(snapshot());
        render_size(&mut a, 30, 8);
        let area = Rect::new(0, 0, 30, 8);
        assert!(!a.hits.is_empty());
        for (r, _) in &a.hits {
            assert_eq!(r.intersection(area), *r, "{r:?}");
        }
    }
}
