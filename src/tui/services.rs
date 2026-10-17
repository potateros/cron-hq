//! The services view (port of `ServicesView.tsx` and the services parts of `App.tsx`): docker compose stacks,
//! per-service state and usage, and the X / Z start-stop actions.

use super::canvas::{between, bold, put, rule, sp, visible, Span};
use super::jobs::draw_footer;
use super::theme::{clock_line, inner_width, ACCENT, BODY, FAULT, HOLD, INDENT, MARGIN};
use super::{panic_text, service_key, App, Effects, Hit, LoopEvent, Mode, Msg, PendingKind};
use crate::stacks::{
    compose_args, format_bytes, is_service_fault, is_up, last_line, service_url, short_uptime, totals, ServiceState,
    ServiceView, Stack, StackView,
};
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::Frame;
use std::time::Instant;
use unicode_width::UnicodeWidthStr;

pub const SERVICE_KEYS: &str = "↑↓ select   O open   X stop/start   Z whole stack   Tab jobs   H help   Q exit";

/// Rows above and below the list in the TS layout; the list gets the rest of the terminal.
const CHROME: usize = 15;

/// Heavier than this is drawn in amber so the culprits stand out.
const HEAVY_CPU: f64 = 50.0;
const HEAVY_MEM: u64 = 1024 * 1024 * 1024;

fn glyph(v: &ServiceView) -> &'static str {
    if is_service_fault(v) {
        "◆"
    } else if is_up(v) {
        "◉"
    } else {
        "○"
    }
}

fn state_name(s: ServiceState) -> &'static str {
    match s {
        ServiceState::Running => "running",
        ServiceState::Restarting => "restarting",
        ServiceState::Paused => "paused",
        ServiceState::Exited => "exited",
        ServiceState::Created => "created",
        ServiceState::Dead => "dead",
        ServiceState::Missing => "missing",
    }
}

fn state_text(v: &ServiceView, pending: Option<PendingKind>) -> String {
    match pending {
        Some(PendingKind::Starting) => return "starting…".into(),
        Some(PendingKind::Stopping) => return "stopping…".into(),
        None => {}
    }
    match v.state {
        ServiceState::Missing => "not created".into(),
        ServiceState::Running => {
            let up = format!("up {}", short_uptime(&v.status));
            if !v.health.is_empty() && v.health != "healthy" {
                format!("{} · {up}", v.health)
            } else {
                up
            }
        }
        ServiceState::Exited => {
            if is_service_fault(v) {
                format!("exit {}", v.exit_code)
            } else {
                "stopped".into()
            }
        }
        s => state_name(s).into(),
    }
}

fn pad_end(s: &str, n: usize) -> String {
    format!("{s}{}", " ".repeat(n.saturating_sub(s.width())))
}

fn pad_start(s: &str, n: usize) -> String {
    format!("{}{s}", " ".repeat(n.saturating_sub(s.width())))
}

fn service_row(v: &ServiceView, selected: bool, pending: Option<PendingKind>) -> Vec<Span> {
    let g = glyph(v);
    let up = is_up(v);
    let g_color = match g {
        "◆" => FAULT,
        "○" => HOLD,
        _ if selected => ACCENT,
        _ => BODY,
    };
    let mut name = Style::default().fg(if selected {
        ACCENT
    } else if up {
        BODY
    } else {
        HOLD
    });
    if selected {
        name = name.add_modifier(Modifier::BOLD);
    }
    let mut state = Style::default().fg(if g == "◆" { FAULT } else { BODY });
    if !up {
        state = state.add_modifier(Modifier::DIM);
    }
    let heavy = v.cpu.unwrap_or(0.0) >= HEAVY_CPU || v.mem.unwrap_or(0) >= HEAVY_MEM;
    let cpu = v.cpu.map(|c| format!("{c:.1}%")).unwrap_or_default();
    let mem = v.mem.map(format_bytes).unwrap_or_default();
    let port = v.ports.first().map(|p| format!("  :{p}")).unwrap_or_default();
    vec![
        sp(g, g_color),
        (format!("  {}", pad_end(&v.service.to_uppercase(), 26)), name),
        (pad_end(&state_text(v, pending), 18), state),
        sp(format!("{}{}", pad_start(&cpu, 7), pad_start(&mem, 8)), if heavy { FAULT } else { BODY }),
        sp(pad_end(&port, 8), HOLD),
    ]
}

fn field(label: &str, value: String) -> Vec<Span> {
    vec![sp(pad_end(label, 10), HOLD), sp(value, BODY)]
}

enum Item<'a> {
    Stack(&'a StackView),
    Svc(&'a ServiceView, usize),
}

/// How many services a Z answered with Y would touch.
fn stack_targets(view: &StackView, start: bool) -> Vec<&ServiceView> {
    view.services.iter().filter(|v| is_up(v) != start).collect()
}

fn find_stack<'a>(stacks: &'a Option<Vec<StackView>>, name: &str) -> Option<&'a StackView> {
    stacks.as_ref()?.iter().find(|s| s.stack.name == name)
}

/// `HQ · SERVICES`: draw the whole view and register its clickable regions.
///
/// The Z prompt replaces the footer line (ruling R22), so it is on screen whenever the footer is. The service
/// rows are windowed around the selection; the detail block is dropped before the footer could be pushed off.
pub fn draw_services(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    let w = inner_width(area.width);
    let rows = area.height as usize;
    let count = app.service_count();
    app.svc_selected = app.svc_selected.min(count.saturating_sub(1));
    let selected = app.svc_selected;
    let banner = app.action_error.clone().or_else(|| app.svc_error.clone()).map(|e| format!("ERROR — {e}"));
    let now = app.now;
    let mut hits: Vec<(Rect, Hit)> = Vec::new();

    let stacks: &[StackView] = app.stacks.as_deref().unwrap_or(&[]);
    let all: Vec<ServiceView> = stacks.iter().flat_map(|s| s.services.iter().cloned()).collect();
    let sum = totals(&all);
    let current = all.get(selected);
    let prompt = match &app.mode {
        Mode::Stack { stack, start } => find_stack(&app.stacks, stack).map(|view| {
            let verb = if *start { "start" } else { "stop" };
            format!("{verb} {} services in {}?  Y confirm · any key cancel", stack_targets(view, *start).len(), stack.to_uppercase())
        }),
        _ => None,
    };

    let mut items: Vec<Item> = Vec::new();
    let mut index = 0;
    for s in stacks {
        items.push(Item::Stack(s));
        for v in &s.services {
            items.push(Item::Svc(v, index));
            index += 1;
        }
    }

    // Vertical budget: rows used above the list, then the list, then [rule + 3 detail lines], rule, footer.
    let head = 5 + usize::from(banner.is_some());
    let room = rows.saturating_sub(CHROME).max(4);
    let below = rows.saturating_sub(head);
    let detail_on = current.is_some() && below.saturating_sub(6) >= 4;
    let room = if detail_on { room.min(below - 6) } else { room.min(below.saturating_sub(2)).max(1) };
    let at = items.iter().position(|i| matches!(i, Item::Svc(_, n) if *n == selected));
    let first = match at {
        Some(at) if items.len() > room => at.saturating_sub(room / 2).min(items.len() - room),
        _ => 0,
    };

    let buf = frame.buffer_mut();
    let mut y: u16 = 1; // paddingTop
    between(buf, area, MARGIN, y, w, &[bold(sp("◢ H Q · SERVICES", ACCENT))], &[sp(clock_line(now), BODY)]);
    y += 1;
    if let Some(b) = &banner {
        put(buf, area, MARGIN, y, w, &[sp(b.clone(), FAULT)]);
        y += 1;
    }
    rule(buf, area, y, w);
    y += 1;

    // UP / CPU / MEM / STACKS spread across the divider's width.
    let summary = [
        (format!("UP {}/{}", sum.up, sum.all), BODY),
        (format!("CPU {:.1}%", sum.cpu), BODY),
        (format!("MEM {}", format_bytes(sum.mem)), BODY),
        (format!("STACKS {}", stacks.len()), HOLD),
    ];
    let total: usize = summary.iter().map(|(t, _)| t.width()).sum();
    let free = (w as usize).saturating_sub(total);
    let mut before = 0;
    for (i, (t, c)) in summary.iter().enumerate() {
        let x = MARGIN + (before + free * i / 3) as u16;
        put(buf, area, x, y, w.saturating_sub(x - MARGIN), &[sp(t.clone(), *c)]);
        before += t.width();
    }
    y += 1;
    rule(buf, area, y, w);
    y += 1;

    if app.stacks.is_none() {
        put(buf, area, MARGIN, y, w, &[sp("reading docker…", HOLD)]);
        y += 1;
    } else if stacks.is_empty() {
        put(buf, area, MARGIN, y, w, &[sp("no stacks — add [stacks.<name>] dir = \"…\" to jobs.toml", HOLD)]);
        y += 1;
    }
    for item in items.iter().skip(first).take(room) {
        match item {
            Item::Svc(v, i) => {
                let pending = app.pending.get(&service_key(&v.stack, &v.service)).map(|(k, _)| *k);
                put(buf, area, MARGIN, y, w, &service_row(v, *i == selected, pending));
                if let Some(r) = visible(Rect::new(MARGIN, y, w, 1), area) {
                    hits.push((r, Hit::Service(*i)));
                }
            }
            Item::Stack(s) => {
                let t = totals(&s.services);
                let label = format!("{} · {}/{} up", s.stack.name.to_uppercase(), t.up, t.all);
                let (right, color) = match &s.error {
                    Some(e) => (e.clone(), FAULT),
                    None if t.up > 0 => (format!("{:.1}%  {}", t.cpu, format_bytes(t.mem)), HOLD),
                    None => (String::new(), HOLD),
                };
                between(buf, area, MARGIN, y, w, &[sp(label, HOLD)], &[sp(right, color)]);
            }
        }
        y += 1;
    }

    if let (true, Some(v)) = (detail_on, current) {
        rule(buf, area, y, w);
        put(buf, area, MARGIN, y + 1, w, &[sp(format!("▸  {}", v.service.to_uppercase()), ACCENT), sp(format!(" — {}", v.image), BODY)]);
        let fx = MARGIN + INDENT;
        let fw = w.saturating_sub(INDENT);
        let url = service_url(v).unwrap_or_else(|| "— (no published port)".to_string());
        put(buf, area, fx, y + 2, fw, &field("url", url));
        let status = format!("{}{}", v.status, v.container.as_ref().map(|c| format!(" · {c}")).unwrap_or_default());
        put(buf, area, fx, y + 3, fw, &field("status", status));
        y += 4;
    }

    rule(buf, area, y, w);
    y += 1;
    match &prompt {
        Some(text) => {
            put(buf, area, MARGIN, y, w, &[sp(text.clone(), FAULT)]);
            let prefix = text.split("Y confirm").next().unwrap_or("").width() as u16;
            let r = Rect::new(MARGIN + prefix, y, "Y confirm".width() as u16, 1);
            if let Some(r) = visible(r.intersection(Rect::new(MARGIN, y, w, 1)), area) {
                hits.push((r, Hit::Confirm));
            }
        }
        None => draw_footer(buf, area, y, w, SERVICE_KEYS, &mut hits),
    }
    app.hits = hits;
}

impl App {
    fn service_at(&self, index: usize) -> Option<&ServiceView> {
        self.stacks.as_ref()?.iter().flat_map(|s| s.services.iter()).nth(index)
    }

    /// Services keys: arrows, O, X, Z.
    pub(super) fn services_key(&mut self, ev: KeyEvent, fx: &dyn Effects) {
        let count = self.service_count();
        let cur = self.svc_selected.min(count.saturating_sub(1));
        match ev.code {
            KeyCode::Up => self.svc_selected = cur.saturating_sub(1),
            KeyCode::Down => self.svc_selected = (cur + 1).min(count.saturating_sub(1)),
            KeyCode::Char(c) => match c.to_ascii_lowercase() {
                'o' => {
                    if let Some(url) = self.service_at(cur).and_then(service_url) {
                        fx.open(&url);
                    }
                }
                'x' => self.toggle_service(cur, fx),
                'z' => {
                    let Some(name) = self.service_at(cur).map(|v| v.stack.clone()) else { return };
                    if let Some(view) = find_stack(&self.stacks, &name) {
                        let start = !view.services.iter().any(is_up);
                        self.mode = Mode::Stack { stack: name, start };
                    }
                }
                _ => {}
            },
            _ => {}
        }
    }

    /// X: stop the selected service if it is up, else start it. Ignored while it is already changing.
    fn toggle_service(&mut self, index: usize, fx: &dyn Effects) {
        let Some(v) = self.service_at(index).cloned() else { return };
        let key = service_key(&v.stack, &v.service);
        if self.pending.contains_key(&key) {
            return;
        }
        let Some(stack) = find_stack(&self.stacks, &v.stack).map(|s| s.stack.clone()) else { return };
        if is_up(&v) {
            self.run_compose(fx, stack, vec!["stop".into(), v.service], vec![key], PendingKind::Stopping);
        } else {
            self.run_compose(fx, stack, vec!["up".into(), "-d".into(), v.service], vec![key], PendingKind::Starting);
        }
    }

    /// Y on a stack prompt: run the compose command and mark the affected rows pending.
    pub(super) fn confirm_stack(&mut self, fx: &dyn Effects) {
        let Mode::Stack { stack, start } = &self.mode else { return };
        let start = *start;
        let Some(view) = find_stack(&self.stacks, stack) else { return };
        let keys: Vec<String> = stack_targets(view, start).iter().map(|v| service_key(&v.stack, &v.service)).collect();
        let s = view.stack.clone();
        let (rest, kind) = if start {
            (vec!["up".to_string(), "-d".to_string()], PendingKind::Starting)
        } else {
            (vec!["stop".to_string()], PendingKind::Stopping)
        };
        self.run_compose(fx, s, rest, keys, kind);
    }

    /// Run a compose command on its own thread (no timeout: a pull can take minutes). Rows show
    /// starting…/stopping… until docker agrees; a failure comes back as `Msg::ActionFailed`.
    fn run_compose(&mut self, fx: &dyn Effects, stack: Stack, rest: Vec<String>, keys: Vec<String>, kind: PendingKind) {
        self.action_error = None;
        let at = Instant::now();
        for k in &keys {
            self.pending.insert(k.clone(), (kind, at));
        }
        let docker = fx.docker();
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let refs: Vec<&str> = rest.iter().map(String::as_str).collect();
            let args = compose_args(&stack, &refs);
            let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match docker.exec(&args, None) {
                Ok(r) if r.code == 0 => Ok(()),
                Ok(r) => {
                    let l = last_line(&r.stderr);
                    Err(if l.is_empty() { format!("exit {}", r.code) } else { l.to_string() })
                }
                Err(e) => Err(e),
            }));
            let msg = match run.unwrap_or_else(|p| Err(format!("worker panicked: {}", panic_text(p)))) {
                Ok(()) => Msg::ActionDone,
                Err(e) => Msg::ActionFailed(format!("{}: {} — {e}", stack.name, rest.join(" ")), keys),
            };
            if let Some(tx) = tx {
                let _ = tx.send(LoopEvent::Msg(msg));
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::launchd::CmdOutput;
    use crate::stacks::{load_stack_views, load_usage, ConfigCache, Docker};
    use crate::tui::testing::{find, lines, now, snapshot, Rec};
    use crate::tui::{draw, handle_event, View};
    use crossterm::event::{KeyEventKind, KeyEventState, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use std::collections::BTreeSet;
    use std::sync::mpsc::{self, Receiver};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    fn k(c: KeyCode) -> KeyEvent {
        KeyEvent { code: c, modifiers: KeyModifiers::NONE, kind: KeyEventKind::Press, state: KeyEventState::NONE }
    }
    fn ch(c: char) -> KeyEvent {
        k(KeyCode::Char(c))
    }
    fn click(col: u16, row: u16) -> MouseEvent {
        MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column: col, row, modifiers: KeyModifiers::NONE }
    }

    /// Fake docker: `stop` / `up` change the running set; every call is recorded with its timeout.
    struct Fake {
        services: Vec<(String, u16)>,
        calls: Mutex<Vec<(Vec<String>, Option<Duration>)>>,
        running: Mutex<BTreeSet<String>>,
        up_error: Option<String>,
        down: bool,
    }

    impl Fake {
        fn new(running: &[&str]) -> Fake {
            let services = vec![("jellyfin".to_string(), 8096), ("radarr".to_string(), 7878)];
            Fake::with(services, running)
        }
        fn with(services: Vec<(String, u16)>, running: &[&str]) -> Fake {
            Fake {
                services,
                calls: Mutex::default(),
                running: Mutex::new(running.iter().map(|s| s.to_string()).collect()),
                up_error: None,
                down: false,
            }
        }
        /// Compose commands that changed something, as the args after `--project-directory /lab`.
        fn actions(&self) -> Vec<Vec<String>> {
            let calls = self.calls.lock().unwrap();
            calls.iter().filter(|(a, _)| a.len() > 3 && ["stop", "up"].contains(&a[3].as_str())).map(|(a, _)| a[3..].to_vec()).collect()
        }
        fn timeout_of(&self, verb: &str) -> Option<Option<Duration>> {
            self.calls.lock().unwrap().iter().find(|(a, _)| a.get(3).map(String::as_str) == Some(verb)).map(|c| c.1)
        }
    }

    impl Docker for Fake {
        fn exec(&self, args: &[String], timeout: Option<Duration>) -> Result<CmdOutput, String> {
            self.calls.lock().unwrap().push((args.to_vec(), timeout));
            if self.down {
                return Err("docker not found".into());
            }
            let out = |stdout: String| Ok(CmdOutput { code: 0, stdout, stderr: String::new() });
            let running = self.running.lock().unwrap().clone();
            if args[0] == "stats" {
                let rows: Vec<String> = running
                    .iter()
                    .map(|s| serde_json::json!({ "Name": s, "CPUPerc": "62.0%", "MemUsage": "1.5GiB / 8GiB" }).to_string())
                    .collect();
                return out(rows.join("\n"));
            }
            let rest = &args[3..];
            match rest[0].as_str() {
                "config" => {
                    let svcs: serde_json::Map<String, serde_json::Value> = self
                        .services
                        .iter()
                        .map(|(n, p)| (n.clone(), serde_json::json!({ "image": format!("img/{n}"), "ports": [{ "target": 80, "published": p.to_string() }] })))
                        .collect();
                    out(serde_json::json!({ "services": svcs }).to_string())
                }
                "ps" => out(running
                    .iter()
                    .map(|s| serde_json::json!({ "Service": s, "Name": s, "State": "running", "Status": "Up 3 hours", "ExitCode": 0 }).to_string())
                    .collect::<Vec<_>>()
                    .join("\n")),
                "stop" => {
                    let mut r = self.running.lock().unwrap();
                    match rest.get(1) {
                        Some(s) => {
                            r.remove(s);
                        }
                        None => r.clear(),
                    }
                    out(String::new())
                }
                "up" => {
                    if let Some(e) = &self.up_error {
                        return Ok(CmdOutput { code: 1, stdout: String::new(), stderr: e.clone() });
                    }
                    let mut r = self.running.lock().unwrap();
                    match rest.get(2) {
                        Some(s) => {
                            r.insert(s.clone());
                        }
                        None => r.extend(self.services.iter().map(|s| s.0.clone())),
                    }
                    out(String::new())
                }
                _ => Ok(CmdOutput { code: 1, stdout: String::new(), stderr: "unexpected".into() }),
            }
        }
    }

    struct H {
        app: App,
        fx: Rec,
        fake: Arc<Fake>,
        rx: Receiver<LoopEvent>,
        cache: ConfigCache,
        stack: Stack,
    }

    impl H {
        fn new(fake: Fake) -> H {
            let fake = Arc::new(fake);
            let (tx, rx) = mpsc::channel();
            let mut app = App::new(now());
            app.tx = Some(tx);
            let fx = Rec { docker: Some(fake.clone()), ..Rec::default() };
            let stack = Stack { name: "lab".into(), dir: "/lab".into(), files: vec![], env_file: None };
            let mut h = H { app, fx, fake, rx, cache: ConfigCache::new(), stack };
            h.reload();
            h
        }
        fn on_services(fake: Fake) -> H {
            let mut h = H::new(fake);
            h.key(k(KeyCode::Tab));
            h
        }
        /// What the stacks and usage workers do.
        fn reload(&mut self) {
            let usage = load_usage(&*self.fake).unwrap_or_default();
            let r = load_stack_views(std::slice::from_ref(&self.stack), &*self.fake, &usage, &mut self.cache);
            self.app.apply(Msg::Usage(usage));
            self.app.apply(Msg::Stacks(r));
        }
        fn key(&mut self, ev: KeyEvent) -> bool {
            self.app.handle_key(ev, &self.fx)
        }
        fn click(&mut self, col: u16, row: u16) -> bool {
            self.app.handle_mouse(click(col, row), &self.fx)
        }
        /// Wait for the action thread's result, apply it, and reload docker like the poked worker would.
        fn settle(&mut self) {
            let mut stale = false;
            let ev = self.rx.recv_timeout(Duration::from_secs(5)).expect("the action reports back");
            assert!(handle_event(&mut self.app, ev, &self.fx, &mut stale));
            if std::mem::take(&mut self.app.refresh.stacks) {
                self.reload();
            }
        }
        fn render_size(&mut self, w: u16, h: u16) -> Vec<String> {
            let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
            t.draw(|f| draw(f, &mut self.app)).unwrap();
            lines(t.backend().buffer())
        }
        fn render(&mut self) -> Vec<String> {
            self.render_size(100, 40)
        }
        fn text(&mut self) -> String {
            self.render().join("\n")
        }
        /// Click the first occurrence of `what` on the drawn screen.
        fn click_on(&mut self, what: &str) -> bool {
            let ls = self.render();
            let (x, y) = find(&ls, what);
            self.click(x, y)
        }
    }

    fn in_order(line: &str, parts: &[&str]) {
        let mut from = 0;
        for p in parts {
            let i = line[from..].find(p).unwrap_or_else(|| panic!("{p:?} after column {from} in {line:?}"));
            from += i + p.len();
        }
    }

    // ─── ports of test/tui/services.test.tsx ───

    #[test]
    fn jobs_view_shows_docker_summary_line_and_the_job_url() {
        let mut h = H::new(Fake::new(&["jellyfin"]));
        let mut snap = snapshot();
        snap.jobs[0].job.url = Some("http://localhost:8742/".into());
        h.app.snap = Some(snap);
        let f = h.text();
        assert!(f.contains("SERVICES 1/2 up · cpu 62.0% · mem 1.5G"), "{f}");
        assert!(f.contains("url       http://localhost:8742/"), "{f}");
        h.key(ch('o'));
        assert_eq!(*h.fx.opened.borrow(), vec!["http://localhost:8742/".to_string()]);
    }

    #[test]
    fn tab_lists_services_with_state_usage_and_port() {
        let mut h = H::on_services(Fake::new(&["jellyfin"]));
        let ls = h.render();
        let f = ls.join("\n");
        for s in ["◢ H Q · SERVICES", "UP 1/2", "LAB · 1/2 up", "url       http://localhost:8096"] {
            assert!(f.contains(s), "{s}\n{f}");
        }
        in_order(ls.iter().find(|l| l.contains("JELLYFIN") && l.contains('◉')).unwrap(), &["◉  JELLYFIN", "up 3h", "62.0%", "1.5G", ":8096"]);
        in_order(ls.iter().find(|l| l.contains("RADARR") && l.contains('○')).unwrap(), &["○  RADARR", "not created"]);
        h.app.snap = Some(snapshot());
        h.key(k(KeyCode::Tab));
        assert!(h.text().contains("SYSTEMS"));
    }

    #[test]
    fn o_opens_the_service_x_stops_or_starts_it() {
        let mut h = H::on_services(Fake::new(&["jellyfin"]));
        h.key(ch('o'));
        assert_eq!(*h.fx.opened.borrow(), vec!["http://localhost:8096".to_string()]);
        h.key(ch('x'));
        assert!(h.text().contains("stopping…"), "pending shows at once");
        h.settle();
        assert_eq!(h.fake.actions(), vec![vec!["stop".to_string(), "jellyfin".into()]]);
        // polls keep docker's default timeout; start/stop run uncapped (a pull can take minutes)
        assert_eq!(h.fake.timeout_of("stop"), Some(None));
        assert_eq!(h.fake.timeout_of("ps"), Some(Some(Duration::from_secs(15))));
        assert_eq!(h.fake.timeout_of("config"), Some(Some(Duration::from_secs(15))));
        assert!(h.app.pending.is_empty());
        h.key(k(KeyCode::Down));
        h.key(ch('x'));
        assert!(h.text().contains("starting…"));
        h.settle();
        assert_eq!(h.fake.actions().last().unwrap(), &["up", "-d", "radarr"]);
        let ls = h.render();
        in_order(ls.iter().find(|l| l.contains("RADARR") && l.contains('◉')).unwrap(), &["RADARR", "up 3h"]);
    }

    #[test]
    fn z_asks_before_stopping_the_whole_stack() {
        let mut h = H::on_services(Fake::new(&["jellyfin", "radarr"]));
        h.key(ch('z'));
        assert!(h.text().contains("stop 2 services in LAB?  Y confirm · any key cancel"));
        h.key(ch('n'));
        assert!(h.fake.actions().is_empty());
        assert!(matches!(h.app.mode, Mode::Dash));
        h.key(ch('z'));
        h.key(ch('y'));
        h.settle();
        assert_eq!(h.fake.actions(), vec![vec!["stop".to_string()]]);
        assert!(h.text().contains("UP 0/2"));
    }

    #[test]
    fn z_on_a_stopped_stack_offers_to_start_it() {
        let mut h = H::on_services(Fake::new(&[]));
        h.key(ch('z'));
        assert!(h.text().contains("start 2 services in LAB?  Y confirm · any key cancel"));
        h.key(ch('y'));
        assert!(h.text().contains("starting…"));
        h.settle();
        assert_eq!(h.fake.actions(), vec![vec!["up".to_string(), "-d".into()]]);
        assert!(h.text().contains("UP 2/2"));
    }

    #[test]
    fn h_shows_services_help_in_the_services_view_and_jobs_help_in_the_jobs_view() {
        let mut h = H::new(Fake::new(&["jellyfin"]));
        h.app.snap = Some(snapshot());
        h.key(ch('h'));
        let f = h.text();
        assert!(f.contains("JOB STATUS") && !f.contains("O · X · Z"), "{f}");
        h.key(k(KeyCode::Esc));
        h.key(k(KeyCode::Tab));
        h.key(ch('h'));
        let f = h.text();
        assert!(f.contains("O · X · Z") && !f.contains("JOB STATUS"), "{f}");
    }

    #[test]
    fn a_failed_compose_command_shows_as_an_error_and_forgets_pending() {
        let mut fake = Fake::new(&["jellyfin"]);
        fake.up_error = Some("warn\nError: drive not mounted\n".into());
        let mut h = H::on_services(fake);
        h.key(k(KeyCode::Down));
        h.key(ch('x'));
        assert_eq!(h.app.pending.len(), 1);
        h.settle();
        assert!(h.text().contains("ERROR — lab: up -d radarr — Error: drive not mounted"));
        assert!(h.app.pending.is_empty(), "the failed key is no longer pending");
        // the error stays until the next action
        h.key(k(KeyCode::Up));
        assert!(h.text().contains("ERROR — lab: up -d radarr"));
        h.key(ch('x'));
        assert!(h.app.action_error.is_none());
        h.settle();
    }

    #[test]
    fn x_ignores_a_service_that_is_already_changing() {
        let mut h = H::on_services(Fake::new(&["jellyfin"]));
        h.key(ch('x'));
        h.key(ch('x'));
        h.settle();
        assert_eq!(h.fake.actions().len(), 1);
    }

    #[test]
    fn exit_codes_and_health_show_in_the_state_column() {
        let mut h = H::on_services(Fake::new(&["jellyfin"]));
        let mut views = h.app.stacks.clone().unwrap();
        views[0].services[0].health = "unhealthy".into();
        views[0].services[1].state = ServiceState::Exited;
        views[0].services[1].exit_code = 2;
        h.app.stacks = Some(views);
        let f = h.text();
        assert!(f.contains("unhealthy · up 3h"), "{f}");
        assert!(f.contains("exit 2"), "{f}");
    }

    // ─── mouse ───

    #[test]
    fn click_selects_service_then_opens_url() {
        let mut h = H::on_services(Fake::new(&["jellyfin", "radarr"]));
        h.click_on("RADARR");
        assert_eq!(h.app.svc_selected, 1);
        assert!(h.fx.opened.borrow().is_empty(), "first click only selects");
        h.click_on("RADARR");
        assert_eq!(*h.fx.opened.borrow(), vec!["http://localhost:7878".to_string()]);
        assert!(h.fake.actions().is_empty(), "a row click never starts or stops");
    }

    #[test]
    fn click_footer_x_stops_service() {
        let mut h = H::on_services(Fake::new(&["jellyfin"]));
        h.click_on("X stop/start");
        assert!(matches!(h.app.mode, Mode::Dash), "no confirmation, same as the key");
        h.settle();
        assert_eq!(h.fake.actions(), vec![vec!["stop".to_string(), "jellyfin".into()]]);
    }

    #[test]
    fn click_footer_z_still_requires_y() {
        let mut h = H::on_services(Fake::new(&["jellyfin", "radarr"]));
        h.click_on("Z whole stack");
        assert!(matches!(h.app.mode, Mode::Stack { .. }));
        assert!(h.fake.actions().is_empty());
        // a click anywhere but Y confirm cancels
        let ls = h.render();
        let (x, y) = find(&ls, "any key cancel");
        h.click(x, y);
        assert!(matches!(h.app.mode, Mode::Dash));
        assert!(h.fake.actions().is_empty());
        h.click_on("Z whole stack");
        h.click_on("Y confirm");
        h.settle();
        assert_eq!(h.fake.actions(), vec![vec!["stop".to_string()]]);
    }

    #[test]
    fn footer_arrows_are_not_clickable_but_tab_and_q_are() {
        let mut h = H::on_services(Fake::new(&["jellyfin"]));
        let ls = h.render();
        let (x, y) = find(&ls, "↑↓ select");
        assert!(h.app.hits.iter().all(|(r, hit)| !(matches!(hit, Hit::Key(_)) && r.y == y && r.x <= x && x < r.x + r.width)));
        h.click_on("Tab jobs");
        assert_eq!(h.app.view, View::Jobs);
        h.key(k(KeyCode::Tab));
        assert!(!h.click_on("Q exit"));
    }

    #[test]
    fn wheel_moves_the_selection() {
        let mut h = H::on_services(Fake::new(&["jellyfin"]));
        let wheel = MouseEvent { kind: MouseEventKind::ScrollDown, column: 0, row: 0, modifiers: KeyModifiers::NONE };
        h.app.handle_mouse(wheel, &h.fx);
        assert_eq!(h.app.svc_selected, 1);
    }

    // ─── layout ───

    fn many(n: usize) -> Fake {
        let services: Vec<(String, u16)> = (0..n).map(|i| (format!("svc{i:02}"), 9000 + i as u16)).collect();
        let running: Vec<String> = services.iter().map(|s| s.0.clone()).collect();
        Fake::with(services, &running.iter().map(String::as_str).collect::<Vec<_>>())
    }

    #[test]
    fn z_prompt_is_visible_at_80x24_even_with_many_services() {
        let mut h = H::on_services(many(30));
        for _ in 0..29 {
            h.key(k(KeyCode::Down));
        }
        h.key(ch('z'));
        let ls = h.render_size(80, 24);
        assert!(ls.iter().any(|l| l.contains("stop 30 services in LAB?  Y confirm · any key cancel")), "{}", ls.join("\n"));
        assert!(ls.iter().any(|l| l.contains("SVC29")), "the selected row is windowed into view\n{}", ls.join("\n"));
        assert!(h.app.hits.iter().any(|(_, hit)| *hit == Hit::Confirm));
        let (x, y) = find(&ls, "Y confirm");
        h.click(x, y);
        h.settle();
        assert_eq!(h.fake.actions(), vec![vec!["stop".to_string()]]);
    }

    #[test]
    fn footer_stays_on_screen_when_the_list_is_long() {
        let mut h = H::on_services(many(30));
        for _ in 0..15 {
            h.key(k(KeyCode::Down));
        }
        let ls = h.render_size(80, 24);
        assert!(ls.iter().any(|l| l.contains("Tab jobs")), "{}", ls.join("\n"));
        assert!(ls.iter().any(|l| l.contains("SVC15")));
        assert_eq!(h.app.hits.iter().filter(|(_, hit)| matches!(hit, Hit::Service(_))).count(), 9);
    }

    #[test]
    fn renders_tiny_terminals_without_panic() {
        let mut h = H::on_services(many(5));
        h.key(ch('z'));
        for (w, ht) in [(0, 0), (1, 1), (20, 5), (10, 3), (30, 8), (80, 24), (200, 60)] {
            h.render_size(w, ht);
        }
        h.app.mode = Mode::Dash;
        h.app.action_error = Some("日本語 failed".into());
        for (w, ht) in [(1, 1), (20, 5), (80, 10)] {
            h.render_size(w, ht);
        }
    }

    #[test]
    fn wide_names_and_errors_truncate_by_width() {
        let mut h = H::on_services(Fake::new(&["jellyfin"]));
        h.app.svc_error = Some("日本語".repeat(40));
        let ls = h.render_size(40, 24);
        assert!(ls.iter().any(|l| l.contains("ERROR — 日本語") && l.trim_end().ends_with('…')), "{}", ls.join("\n"));
    }

    #[test]
    fn empty_and_loading_states() {
        let mut app = App::new(now());
        app.view = View::Services;
        let mut t = Terminal::new(TestBackend::new(100, 24)).unwrap();
        t.draw(|f| draw(f, &mut app)).unwrap();
        assert!(lines(t.backend().buffer()).join("\n").contains("reading docker…"));
        app.apply(Msg::Stacks(Ok(vec![])));
        t.draw(|f| draw(f, &mut app)).unwrap();
        let f = lines(t.backend().buffer()).join("\n");
        assert!(f.contains("no stacks — add [stacks.<name>] dir = \"…\" to jobs.toml"), "{f}");
        assert!(app.hits.iter().all(|(_, h)| matches!(h, Hit::Key(_))), "no service rows to click");
        let fx = Rec::default();
        for c in ['x', 'z', 'o'] {
            app.handle_key(ch(c), &fx);
        }
        assert!(matches!(app.mode, Mode::Dash) && fx.opened.borrow().is_empty());
    }

    // ─── docker trouble ───

    #[test]
    fn docker_missing_shows_error_tui_stays_up() {
        let mut fake = Fake::new(&[]);
        fake.down = true;
        let mut h = H::on_services(fake);
        h.app.snap = Some(snapshot());
        assert!(h.text().contains("ERROR — docker not found"));
        // keys keep working
        assert!(h.key(ch('x')) && h.key(ch('z')) && h.key(ch('o')), "service keys do nothing, harmlessly");
        assert!(matches!(h.app.mode, Mode::Dash));
        h.key(k(KeyCode::Tab));
        assert_eq!(h.app.view, View::Jobs);
        assert!(h.text().contains("SYSTEMS"));
        h.key(ch('h'));
        assert!(matches!(h.app.mode, Mode::Help));
        h.key(k(KeyCode::Esc));
        h.key(k(KeyCode::Tab));
        assert!(h.text().contains("ERROR — docker not found"));
        assert!(!h.key(ch('q')), "Q still quits");
    }

    #[test]
    fn a_later_good_poll_clears_the_docker_error() {
        let mut h = H::on_services(Fake::new(&["jellyfin"]));
        h.app.apply(Msg::Stacks(Err("docker not found".into())));
        assert!(h.text().contains("ERROR — docker not found"));
        h.reload();
        assert!(!h.text().contains("ERROR"));
    }

    #[test]
    fn a_panicking_docker_is_reported_not_fatal() {
        struct Boom;
        impl Docker for Boom {
            fn exec(&self, _: &[String], _: Option<Duration>) -> Result<CmdOutput, String> {
                panic!("kaboom")
            }
        }
        let mut h = H::on_services(Fake::new(&["jellyfin"]));
        let fx = Rec { docker: Some(Arc::new(Boom)), ..Rec::default() };
        h.app.handle_key(ch('x'), &fx);
        let mut stale = false;
        let ev = h.rx.recv_timeout(Duration::from_secs(5)).unwrap();
        handle_event(&mut h.app, ev, &fx, &mut stale);
        let e = h.app.action_error.clone().unwrap();
        assert!(e.contains("kaboom") && e.starts_with("lab: stop jellyfin — "), "{e}");
        assert!(h.app.pending.is_empty());
    }

    #[test]
    fn pending_clears_when_docker_agrees_and_expires() {
        let mut h = H::on_services(Fake::new(&["jellyfin"]));
        h.app.pending.insert("lab/radarr".into(), (PendingKind::Starting, Instant::now()));
        h.reload();
        assert_eq!(h.app.pending.len(), 1, "still not up");
        h.app.pending.insert("lab/radarr".into(), (PendingKind::Starting, Instant::now() - Duration::from_secs(121)));
        h.reload();
        assert!(h.app.pending.is_empty(), "forgotten after 120 s");
    }
}
