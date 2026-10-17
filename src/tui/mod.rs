//! The terminal UI: event loop, `App` state, global keys, mouse dispatch and the worker threads.
//! Views register their clickable regions in `App::hits` while drawing; everything else lives here.

pub mod theme;

use crate::config::load_config;
use crate::history::History;
use crate::launchd::{launchctl_from_env, AgentState, SyncPlan};
use crate::paths::Paths;
use crate::snapshot::{fetch_agents, load_snapshot, Snapshot};
use crate::stacks::{
    is_up, load_stack_view, load_stacks, load_usage, totals, ConfigCache, Docker, RealDocker, Stack, StackView, Usage,
};
use chrono::{DateTime, Local, Timelike};
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton,
    MouseEvent, MouseEventKind,
};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;
use ratatui::widgets::Paragraph;
use ratatui::{Frame, Terminal};
use std::collections::HashMap;
use std::io::{self, Write};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex, Once};
use std::time::{Duration, Instant, SystemTime};

// ─── state ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Jobs,
    Services,
}

#[derive(Debug)]
pub enum Mode {
    Dash,
    Help,
    /// Log of this job id.
    Log(String),
    /// A sync preview awaiting Y; Err is the config/planning error to show.
    Sync(Result<SyncPlan, String>),
    /// Start (or stop) every service of `stack` after Y.
    Stack { stack: String, start: bool },
}

/// A clickable region registered by a view's draw function.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hit {
    Job(usize),
    LogLine(usize),
    Service(usize),
    /// A footer `<key> <label>` segment: acts exactly like pressing the key.
    Key(KeyCode),
    /// The `Y confirm` segment of a prompt.
    Confirm,
    /// The `Tab ▸` hint on the SERVICES line.
    ToggleView,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingKind {
    Starting,
    Stopping,
}

/// A start/stop that never shows up in docker is forgotten after this long.
pub const PENDING_TTL: Duration = Duration::from_secs(120);

/// What worker threads and background actions report to the event loop.
pub enum Msg {
    Snapshot(Snapshot),
    Agents(HashMap<String, AgentState>),
    Stacks(Result<Vec<StackView>, String>),
    Usage(HashMap<String, Usage>),
    ActionFailed(String),
}

/// Side effects the keys can ask for; faked in tests.
pub trait Effects {
    /// Run `hq <args>` detached with null stdio.
    fn spawn_cli(&self, args: &[&str]);
    fn open(&self, url: &str);
    fn docker(&self) -> Arc<dyn Docker>;
}

/// Refreshes views ask the event loop to trigger.
#[derive(Debug, Default)]
pub struct Refresh {
    /// Re-run `launchctl print` soon (after S, P or R).
    pub agents: bool,
    /// Re-run `docker compose ps` now (after a start/stop).
    pub stacks: bool,
}

pub struct App {
    pub view: View,
    pub mode: Mode,
    pub snap: Option<Snapshot>,
    /// Selected job row and service row.
    pub selected: usize,
    pub svc_selected: usize,
    /// Selected log line (the log view owns what it means).
    pub log_cursor: usize,
    /// `None` until docker has answered once.
    pub stacks: Option<Vec<StackView>>,
    pub usage: HashMap<String, Usage>,
    pub agents: HashMap<String, AgentState>,
    pub svc_error: Option<String>,
    /// Stays until the next action.
    pub action_error: Option<String>,
    /// `history.db` could not be opened; rows are shown empty.
    pub history_open_error: Option<String>,
    /// `ERROR — <e>` banner text for the views (snapshot or open failure).
    pub history_error: Option<String>,
    /// `<stack>/<service>` → in-flight start/stop.
    pub pending: HashMap<String, (PendingKind, Instant)>,
    /// Clickable regions of the last draw; later entries are drawn on top.
    pub hits: Vec<(Rect, Hit)>,
    pub now: DateTime<Local>,
    pub refresh: Refresh,
    /// The snapshot should be rebuilt (new agent states arrived).
    pub need_reload: bool,
    pub dirty: bool,
}

pub fn service_key(stack: &str, service: &str) -> String {
    format!("{stack}/{service}")
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

impl App {
    pub fn new(now: DateTime<Local>) -> App {
        App {
            view: View::Jobs,
            mode: Mode::Dash,
            snap: None,
            selected: 0,
            svc_selected: 0,
            log_cursor: 0,
            stacks: None,
            usage: HashMap::new(),
            agents: HashMap::new(),
            svc_error: None,
            action_error: None,
            history_open_error: None,
            history_error: None,
            pending: HashMap::new(),
            hits: Vec::new(),
            now,
            refresh: Refresh::default(),
            need_reload: false,
            dirty: true,
        }
    }

    pub fn apply(&mut self, msg: Msg) {
        self.dirty = true;
        match msg {
            Msg::Snapshot(s) => {
                self.history_error = self.history_open_error.clone().or_else(|| s.history_error.clone());
                self.snap = Some(s);
            }
            Msg::Agents(a) => {
                self.agents = a;
                self.need_reload = true;
            }
            Msg::Stacks(Ok(views)) => {
                self.svc_error = None;
                let live: HashMap<String, bool> = views
                    .iter()
                    .flat_map(|v| v.services.iter())
                    .map(|v| (service_key(&v.stack, &v.service), is_up(v)))
                    .collect();
                let now = Instant::now();
                self.pending.retain(|k, (kind, at)| {
                    let done = match live.get(k) {
                        None => true,
                        Some(&up) => up == (*kind == PendingKind::Starting),
                    };
                    !done && now.duration_since(*at) <= PENDING_TTL
                });
                self.stacks = Some(views);
            }
            Msg::Stacks(Err(e)) => {
                self.svc_error = Some(e);
                if self.stacks.is_none() {
                    self.stacks = Some(Vec::new());
                }
            }
            Msg::Usage(u) => self.usage = u,
            Msg::ActionFailed(e) => self.action_error = Some(e),
        }
    }

    /// `ERROR — <e>` for the history problem, if any.
    pub fn history_banner(&self) -> Option<String> {
        self.history_error.as_ref().map(|e| format!("ERROR — {e}"))
    }

    pub fn service_count(&self) -> usize {
        self.stacks.as_ref().map_or(0, |s| s.iter().map(|v| v.services.len()).sum())
    }

    /// `3/5 up · cpu 1.2% · mem 40.1MiB` for the jobs view's SERVICES line.
    pub fn docker_line(&self) -> Option<String> {
        let stacks = self.stacks.as_ref().filter(|s| !s.is_empty())?;
        let all: Vec<_> = stacks.iter().flat_map(|s| s.services.iter().cloned()).collect();
        let t = totals(&all);
        Some(format!("{}/{} up · cpu {:.1}% · mem {}", t.up, t.all, t.cpu, crate::stacks::format_bytes(t.mem)))
    }

    fn switch_view(&mut self) {
        self.view = match self.view {
            View::Jobs => View::Services,
            View::Services => View::Jobs,
        };
        self.refresh.stacks = true;
    }

    /// Returns false to quit.
    pub fn handle_key(&mut self, ev: KeyEvent, fx: &dyn Effects) -> bool {
        self.dirty = true;
        if ev.modifiers.contains(KeyModifiers::CONTROL) && ev.code == KeyCode::Char('c') {
            return false;
        }
        let lower = match ev.code {
            KeyCode::Char(c) => Some(c.to_ascii_lowercase()),
            _ => None,
        };
        match &self.mode {
            Mode::Log(_) => {
                if ev.code == KeyCode::Esc {
                    self.mode = Mode::Dash;
                } else {
                    self.log_key(ev, fx);
                }
                return true;
            }
            Mode::Help => {
                if ev.code == KeyCode::Esc || lower == Some('h') {
                    self.mode = Mode::Dash;
                }
                return true;
            }
            Mode::Sync(_) => {
                if lower == Some('y') {
                    if let Mode::Sync(Ok(_)) = &self.mode {
                        fx.spawn_cli(&["sync", "--yes"]);
                        self.refresh.agents = true;
                    }
                }
                self.mode = Mode::Dash;
                return true;
            }
            Mode::Stack { .. } => {
                if lower == Some('y') {
                    self.confirm_stack(fx);
                }
                self.mode = Mode::Dash;
                return true;
            }
            Mode::Dash => {}
        }
        if ev.code == KeyCode::Tab {
            self.switch_view();
            return true;
        }
        match lower {
            Some('h') => {
                self.mode = Mode::Help;
                return true;
            }
            Some('q') => return false,
            _ => {}
        }
        match self.view {
            View::Jobs => self.jobs_key(ev, fx),
            View::Services => self.services_key(ev, fx),
        }
        true
    }

    // Extension points: Tasks 8-10 fill these in.

    /// Jobs dash keys: arrows, Enter, R, P, O, S.
    fn jobs_key(&mut self, _ev: KeyEvent, _fx: &dyn Effects) {}

    /// Services keys: arrows, O, X, Z.
    fn services_key(&mut self, _ev: KeyEvent, _fx: &dyn Effects) {}

    /// Keys inside the log view other than Esc.
    fn log_key(&mut self, _ev: KeyEvent, _fx: &dyn Effects) {}

    /// Y on a stack prompt: run the compose command and mark the affected rows pending.
    fn confirm_stack(&mut self, _fx: &dyn Effects) {}

    /// Returns false to quit. Mouse handling lives here only; views just register `hits`.
    pub fn handle_mouse(&mut self, ev: MouseEvent, fx: &dyn Effects) -> bool {
        match ev.kind {
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                let code = if ev.kind == MouseEventKind::ScrollUp { KeyCode::Up } else { KeyCode::Down };
                match self.mode {
                    Mode::Log(_) => {
                        for _ in 0..3 {
                            self.handle_key(key(code), fx);
                        }
                    }
                    Mode::Dash => {
                        self.handle_key(key(code), fx);
                    }
                    // Scrolling never closes or answers a prompt.
                    _ => {}
                }
                true
            }
            MouseEventKind::Down(MouseButton::Left) => self.click(ev.column, ev.row, fx),
            _ => true,
        }
    }

    fn hit_at(&self, col: u16, row: u16) -> Option<Hit> {
        self.hits.iter().rev().find(|(r, _)| col >= r.x && col < r.x + r.width && row >= r.y && row < r.y + r.height).map(|(_, h)| *h)
    }

    fn click(&mut self, col: u16, row: u16, fx: &dyn Effects) -> bool {
        self.dirty = true;
        let hit = self.hit_at(col, row);
        match self.mode {
            Mode::Help => {
                self.mode = Mode::Dash;
                return true;
            }
            Mode::Sync(_) | Mode::Stack { .. } => {
                // Only the Y confirm segment answers; it goes through the key path so nothing is bypassed.
                return if hit == Some(Hit::Confirm) {
                    self.handle_key(key(KeyCode::Char('y')), fx)
                } else {
                    self.mode = Mode::Dash;
                    true
                };
            }
            _ => {}
        }
        match hit {
            Some(Hit::Job(i)) => {
                if i == self.selected {
                    return self.handle_key(key(KeyCode::Enter), fx);
                }
                self.selected = i;
            }
            Some(Hit::LogLine(i)) => {
                if i == self.log_cursor {
                    return self.handle_key(key(KeyCode::Enter), fx);
                }
                self.log_cursor = i;
            }
            Some(Hit::Service(i)) => {
                if i == self.svc_selected {
                    return self.handle_key(key(KeyCode::Char('o')), fx);
                }
                self.svc_selected = i;
            }
            Some(Hit::Key(code)) => return self.handle_key(key(code), fx),
            Some(Hit::ToggleView) => return self.handle_key(key(KeyCode::Tab), fx),
            // A Confirm segment with no prompt open is stale.
            Some(Hit::Confirm) | None => {}
        }
        true
    }
}

// ─── real effects ────────────────────────────────────────────────────────────

pub struct RealEffects {
    docker: Arc<dyn Docker>,
}

impl RealEffects {
    pub fn new() -> RealEffects {
        RealEffects { docker: Arc::new(RealDocker) }
    }
}

fn spawn_detached(mut cmd: Command) {
    use std::os::unix::process::CommandExt;
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).process_group(0);
    if let Ok(mut child) = cmd.spawn() {
        // Reap it so it does not linger as a zombie while the TUI runs.
        std::thread::spawn(move || {
            let _ = child.wait();
        });
    }
}

impl Effects for RealEffects {
    fn spawn_cli(&self, args: &[&str]) {
        if let Ok(exe) = std::env::current_exe() {
            let mut c = Command::new(exe);
            c.args(args);
            spawn_detached(c);
        }
    }
    fn open(&self, url: &str) {
        let mut c = Command::new("/usr/bin/open");
        c.arg(url);
        spawn_detached(c);
    }
    fn docker(&self) -> Arc<dyn Docker> {
        self.docker.clone()
    }
}

// ─── drawing (views replace these stubs in Tasks 8-10) ───────────────────────

pub fn draw(frame: &mut Frame, app: &mut App) {
    app.hits.clear();
    match (&app.mode, app.view) {
        (Mode::Log(_), _) => draw_log(frame, app),
        (Mode::Help, _) => draw_help(frame, app),
        (_, View::Jobs) => draw_jobs(frame, app),
        (_, View::Services) => draw_services(frame, app),
    }
}

fn draw_title(frame: &mut Frame, title: &str) {
    frame.render_widget(Paragraph::new(title.to_string()), frame.area());
}

fn draw_jobs(frame: &mut Frame, _app: &mut App) {
    draw_title(frame, "HQ · JOBS");
}
fn draw_log(frame: &mut Frame, _app: &mut App) {
    draw_title(frame, "HQ · LOG");
}
fn draw_services(frame: &mut Frame, _app: &mut App) {
    draw_title(frame, "HQ · SERVICES");
}
fn draw_help(frame: &mut Frame, _app: &mut App) {
    draw_title(frame, "HQ · HELP");
}

// ─── terminal ────────────────────────────────────────────────────────────────

static TERMINAL_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Undo `setup_terminal`. Safe to call any number of times, from the panic hook as well as on exit:
/// only the first call after setup does anything.
pub fn restore_terminal() -> bool {
    restore_to(&mut io::stdout())
}

fn restore_to(out: &mut impl Write) -> bool {
    if !TERMINAL_ACTIVE.swap(false, Ordering::SeqCst) {
        return false;
    }
    let _ = disable_raw_mode();
    let _ = crossterm::execute!(out, DisableMouseCapture, LeaveAlternateScreen, crossterm::cursor::Show);
    let _ = out.flush();
    true
}

fn setup_terminal() -> io::Result<Terminal<CrosstermBackend<io::Stdout>>> {
    enable_raw_mode()?;
    TERMINAL_ACTIVE.store(true, Ordering::SeqCst);
    let mut out = io::stdout();
    if let Err(e) = crossterm::execute!(out, EnterAlternateScreen, EnableMouseCapture) {
        restore_terminal();
        return Err(e);
    }
    Terminal::new(CrosstermBackend::new(out)).inspect_err(|_| {
        restore_terminal();
    })
}

/// Make a panic leave the terminal usable before the message is printed.
pub fn install_panic_hook() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            restore_terminal();
            prev(info);
        }));
    });
}

// ─── workers ─────────────────────────────────────────────────────────────────

const AGENTS: usize = 0;
const STACKS: usize = 1;
const USAGE: usize = 2;

struct Ctl {
    services_view: bool,
    quit: bool,
    /// Per worker: run no later than this.
    due: [Option<Instant>; 3],
}

/// What the workers sleep on: a view switch or an action wakes them early.
struct Control {
    m: Mutex<Ctl>,
    cv: Condvar,
}

impl Control {
    fn new() -> Arc<Control> {
        Arc::new(Control { m: Mutex::new(Ctl { services_view: false, quit: false, due: [None; 3] }), cv: Condvar::new() })
    }
    fn lock(&self) -> std::sync::MutexGuard<'_, Ctl> {
        self.m.lock().unwrap_or_else(|e| e.into_inner())
    }
    fn poke(&self, worker: usize, after: Duration) {
        let at = Instant::now() + after;
        let mut g = self.lock();
        g.due[worker] = Some(g.due[worker].map_or(at, |d| d.min(at)));
        self.cv.notify_all();
    }
    fn set_view(&self, view: View) {
        let mut g = self.lock();
        g.services_view = view == View::Services;
        self.cv.notify_all();
    }
    fn quit(&self) {
        self.lock().quit = true;
        self.cv.notify_all();
    }
}

fn period(worker: usize, services_view: bool) -> Duration {
    Duration::from_secs(match (worker, services_view) {
        (AGENTS, _) => 30,
        (STACKS, true) => 3,
        (STACKS, false) => 15,
        (_, true) => 6,
        (_, false) => 30,
    })
}

/// Run `job` now, then every `period` (which depends on the view) or when poked; sleeps on the condvar between.
fn spawn_worker(ctl: Arc<Control>, idx: usize, mut job: impl FnMut() + Send + 'static) {
    std::thread::spawn(move || {
        let mut last: Option<Instant> = None;
        loop {
            {
                let mut g = ctl.lock();
                loop {
                    if g.quit {
                        return;
                    }
                    let now = Instant::now();
                    let periodic = last.map_or(now, |l| l + period(idx, g.services_view));
                    let deadline = g.due[idx].map_or(periodic, |d| d.min(periodic));
                    if now >= deadline {
                        g.due[idx] = None;
                        break;
                    }
                    g = ctl.cv.wait_timeout(g, deadline - now).unwrap_or_else(|e| e.into_inner()).0;
                }
            }
            job();
            last = Some(Instant::now());
        }
    });
}

type StackResult = Result<Vec<Stack>, String>;

/// The stacks from `jobs.toml`, re-parsed only when the file's mtime changes.
struct StackList {
    path: std::path::PathBuf,
    cached: Option<(Option<SystemTime>, StackResult)>,
}

impl StackList {
    fn new(path: std::path::PathBuf) -> StackList {
        StackList { path, cached: None }
    }
    fn get(&mut self) -> StackResult {
        let m = mtime(&self.path);
        if self.cached.as_ref().map(|c| c.0) != Some(m) {
            self.cached = Some((m, load_stacks(&self.path).map_err(|e| e.0)));
        }
        self.cached.as_ref().map(|c| c.1.clone()).unwrap_or_else(|| Ok(Vec::new()))
    }
}

fn start_workers(ctl: &Arc<Control>, paths: &Paths, tx: &Sender<Msg>, docker: Arc<dyn Docker>) {
    let usage: Arc<Mutex<HashMap<String, Usage>>> = Arc::default();

    let (p, t) = (paths.clone(), tx.clone());
    spawn_worker(ctl.clone(), AGENTS, move || {
        let lc = launchctl_from_env(&p);
        if let Ok(jobs) = load_config(&p.config) {
            let _ = t.send(Msg::Agents(fetch_agents(&jobs, lc.as_ref())));
        }
    });

    let (t, d, u) = (tx.clone(), docker.clone(), usage.clone());
    let mut list = StackList::new(paths.config.clone());
    let mut cache = ConfigCache::new();
    spawn_worker(ctl.clone(), STACKS, move || {
        let msg = list.get().map(|stacks| {
            let u = u.lock().unwrap_or_else(|e| e.into_inner()).clone();
            stacks.iter().map(|s| load_stack_view(s, d.as_ref(), &u, &mut cache)).collect::<Vec<_>>()
        });
        let _ = t.send(Msg::Stacks(msg));
    });

    let (t, c) = (tx.clone(), ctl.clone());
    let mut list = StackList::new(paths.config.clone());
    spawn_worker(ctl.clone(), USAGE, move || {
        // docker down or stats failing: the ps-based rows still show state, just no CPU/MEM
        if !list.get().is_ok_and(|s| !s.is_empty()) {
            return;
        }
        if let Ok(map) = load_usage(docker.as_ref()) {
            *usage.lock().unwrap_or_else(|e| e.into_inner()) = map.clone();
            let _ = t.send(Msg::Usage(map));
            c.poke(STACKS, Duration::ZERO);
        }
    });
}

// ─── event loop ──────────────────────────────────────────────────────────────

fn mtime(p: &std::path::Path) -> Option<SystemTime> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}

/// What the snapshot is built from, as stat mtimes: config, history.db (+ WAL), state.json, the agents dir.
fn stamps(paths: &Paths) -> [Option<SystemTime>; 5] {
    let db = paths.db();
    let mut wal = db.clone().into_os_string();
    wal.push("-wal");
    [mtime(&paths.config), mtime(&db), mtime(std::path::Path::new(&wal)), mtime(&paths.state()), mtime(&paths.agents)]
}

fn open_history(paths: &Paths) -> (Option<History>, Option<String>) {
    match History::open(&paths.db()) {
        Ok(h) => (Some(h), None),
        Err(e) => match History::in_memory() {
            Ok(h) => (Some(h), Some(e.to_string())),
            Err(e2) => (None, Some(format!("{e}; {e2}"))),
        },
    }
}

fn until_next_second() -> Duration {
    let ns = Local::now().nanosecond().min(999_999_999);
    // +1 ms so we wake just after the boundary, not just before it.
    Duration::from_nanos(1_000_000_000 - ns as u64) + Duration::from_millis(1)
}

pub fn run(paths: Paths) -> io::Result<()> {
    install_panic_hook();
    let mut terminal = setup_terminal()?;
    let res = event_loop(&mut terminal, &paths);
    restore_terminal();
    res
}

fn event_loop(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>, paths: &Paths) -> io::Result<()> {
    let fx = RealEffects::new();
    let ctl = Control::new();
    let (tx, rx): (Sender<Msg>, Receiver<Msg>) = mpsc::channel();
    start_workers(&ctl, paths, &tx, fx.docker());

    let (history, open_err) = open_history(paths);
    let mut app = App::new(Local::now());
    app.history_open_error = open_err;
    let mut seen = [None; 5];
    let mut sent_view = app.view;
    let mut last_second = app.now.second();
    app.need_reload = true;

    let result = (|| -> io::Result<()> {
        loop {
            app.now = Local::now();
            // Drain whatever the workers produced.
            while let Ok(m) = rx.try_recv() {
                app.apply(m);
            }
            let now_stamps = stamps(paths);
            if now_stamps != seen || app.need_reload {
                seen = now_stamps;
                app.need_reload = false;
                if let Some(h) = &history {
                    let s = load_snapshot(paths, h, &app.agents, app.now, app.snap.as_ref());
                    app.apply(Msg::Snapshot(s));
                }
            }
            if app.now.second() != last_second {
                last_second = app.now.second();
                app.dirty = true;
            }
            if app.view != sent_view {
                sent_view = app.view;
                ctl.set_view(sent_view);
            }
            if std::mem::take(&mut app.refresh.agents) {
                ctl.poke(AGENTS, Duration::from_millis(1500));
            }
            if std::mem::take(&mut app.refresh.stacks) {
                ctl.poke(STACKS, Duration::ZERO);
            }
            if std::mem::take(&mut app.dirty) {
                terminal.draw(|f| draw(f, &mut app))?;
            }

            // Sleep until input or the next whole second; nothing else polls.
            if event::poll(until_next_second())? {
                loop {
                    match event::read()? {
                        Event::Key(k) if k.kind != KeyEventKind::Release => {
                            if !app.handle_key(k, &fx) {
                                return Ok(());
                            }
                        }
                        Event::Mouse(m) => {
                            if !app.handle_mouse(m, &fx) {
                                return Ok(());
                            }
                        }
                        Event::Resize(..) => app.dirty = true,
                        _ => {}
                    }
                    if !event::poll(Duration::ZERO)? {
                        break;
                    }
                }
            }
        }
    })();
    ctl.quit();
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyEventState;
    use std::cell::RefCell;

    #[derive(Default)]
    struct Rec {
        cli: RefCell<Vec<Vec<String>>>,
        opened: RefCell<Vec<String>>,
    }
    impl Effects for Rec {
        fn spawn_cli(&self, args: &[&str]) {
            self.cli.borrow_mut().push(args.iter().map(|s| s.to_string()).collect());
        }
        fn open(&self, url: &str) {
            self.opened.borrow_mut().push(url.to_string());
        }
        fn docker(&self) -> Arc<dyn Docker> {
            Arc::new(RealDocker)
        }
    }

    fn app() -> App {
        App::new(Local::now())
    }
    fn k(c: KeyCode) -> KeyEvent {
        KeyEvent { code: c, modifiers: KeyModifiers::NONE, kind: KeyEventKind::Press, state: KeyEventState::NONE }
    }
    fn click(col: u16, row: u16) -> MouseEvent {
        MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column: col, row, modifiers: KeyModifiers::NONE }
    }
    fn scroll(up: bool) -> MouseEvent {
        let kind = if up { MouseEventKind::ScrollUp } else { MouseEventKind::ScrollDown };
        MouseEvent { kind, column: 0, row: 0, modifiers: KeyModifiers::NONE }
    }

    #[test]
    fn global_keys() {
        let (mut a, fx) = (app(), Rec::default());
        assert!(a.handle_key(k(KeyCode::Tab), &fx));
        assert_eq!(a.view, View::Services);
        assert!(a.refresh.stacks);
        a.handle_key(k(KeyCode::Tab), &fx);
        assert_eq!(a.view, View::Jobs);
        a.handle_key(k(KeyCode::Char('H')), &fx);
        assert!(matches!(a.mode, Mode::Help));
        assert!(a.handle_key(k(KeyCode::Char('q')), &fx), "q does nothing inside help");
        a.handle_key(k(KeyCode::Esc), &fx);
        assert!(matches!(a.mode, Mode::Dash));
        a.handle_key(k(KeyCode::Char('h')), &fx);
        a.handle_key(k(KeyCode::Char('h')), &fx);
        assert!(matches!(a.mode, Mode::Dash));
        assert!(!a.handle_key(k(KeyCode::Char('Q')), &fx));
        let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert!(!a.handle_key(ctrl_c, &fx));
    }

    #[test]
    fn sync_prompt_confirms_only_with_y_and_a_plan() {
        let (mut a, fx) = (app(), Rec::default());
        let plan = || SyncPlan { add: vec!["a".into()], change: vec![], remove: vec![] };
        a.mode = Mode::Sync(Ok(plan()));
        a.handle_key(k(KeyCode::Char('x')), &fx);
        assert!(matches!(a.mode, Mode::Dash));
        assert!(fx.cli.borrow().is_empty());
        a.mode = Mode::Sync(Err("bad".into()));
        a.handle_key(k(KeyCode::Char('y')), &fx);
        assert!(fx.cli.borrow().is_empty());
        a.mode = Mode::Sync(Ok(plan()));
        a.handle_key(k(KeyCode::Char('Y')), &fx);
        assert_eq!(*fx.cli.borrow(), vec![vec!["sync".to_string(), "--yes".to_string()]]);
        assert!(a.refresh.agents);
    }

    #[test]
    fn mouse_click_dispatches_hits() {
        let (mut a, fx) = (app(), Rec::default());
        a.hits = vec![
            (Rect::new(0, 0, 40, 1), Hit::Job(0)),
            (Rect::new(0, 1, 40, 1), Hit::Job(1)),
            (Rect::new(0, 9, 5, 1), Hit::Key(KeyCode::Char('q'))),
        ];
        assert!(a.handle_mouse(click(50, 5), &fx), "outside every rect: nothing happens");
        assert_eq!(a.selected, 0);
        assert!(a.handle_mouse(click(3, 1), &fx));
        assert_eq!(a.selected, 1, "first click selects");
        assert!(!a.handle_mouse(click(4, 9), &fx), "q segment quits");
    }

    #[test]
    fn topmost_hit_wins_and_selected_click_activates() {
        let (mut a, fx) = (app(), Rec::default());
        a.hits = vec![(Rect::new(0, 0, 10, 3), Hit::Job(2)), (Rect::new(0, 0, 10, 1), Hit::ToggleView)];
        a.handle_mouse(click(1, 0), &fx);
        assert_eq!(a.view, View::Services, "later hit is on top");
        assert_eq!(a.selected, 0);
        a.handle_mouse(click(1, 2), &fx);
        assert_eq!(a.selected, 2);
        a.hits = vec![(Rect::new(0, 0, 10, 1), Hit::Service(1))];
        a.handle_mouse(click(1, 0), &fx);
        assert_eq!(a.svc_selected, 1);
    }

    #[test]
    fn prompts_need_the_confirm_segment_help_closes_on_any_click() {
        let (mut a, fx) = (app(), Rec::default());
        a.hits = vec![(Rect::new(0, 5, 9, 1), Hit::Confirm), (Rect::new(0, 0, 9, 1), Hit::Job(1))];
        let plan = || Mode::Sync(Ok(SyncPlan { add: vec!["a".into()], change: vec![], remove: vec![] }));
        a.mode = plan();
        a.handle_mouse(click(1, 0), &fx);
        assert!(matches!(a.mode, Mode::Dash), "click elsewhere cancels");
        assert!(fx.cli.borrow().is_empty());
        assert_eq!(a.selected, 0, "and does not fall through to the row");
        a.mode = plan();
        a.handle_mouse(click(1, 5), &fx);
        assert!(matches!(a.mode, Mode::Dash));
        assert_eq!(fx.cli.borrow().len(), 1, "Confirm acts like Y");
        a.mode = Mode::Stack { stack: "s".into(), start: true };
        a.handle_mouse(click(30, 30), &fx);
        assert!(matches!(a.mode, Mode::Dash));
        a.mode = Mode::Help;
        a.handle_mouse(click(30, 30), &fx);
        assert!(matches!(a.mode, Mode::Dash));
        a.mode = plan();
        a.handle_mouse(scroll(false), &fx);
        assert!(matches!(a.mode, Mode::Sync(_)), "scroll does not answer a prompt");
    }

    #[test]
    fn scroll_maps_to_arrow_keys() {
        let (mut a, fx) = (app(), Rec::default());
        // No view keys exist yet, so observe via a mode the global handler reacts to: help ignores scroll,
        // dash forwards it. Both must simply not panic or quit.
        assert!(a.handle_mouse(scroll(true), &fx));
        a.mode = Mode::Log("x".into());
        assert!(a.handle_mouse(scroll(false), &fx));
        assert!(matches!(a.mode, Mode::Log(_)));
        a.mode = Mode::Help;
        assert!(a.handle_mouse(scroll(true), &fx));
        assert!(matches!(a.mode, Mode::Help));
    }

    #[test]
    fn apply_updates_state() {
        let mut a = app();
        a.apply(Msg::ActionFailed("boom".into()));
        assert_eq!(a.action_error.as_deref(), Some("boom"));
        a.apply(Msg::Stacks(Err("docker down".into())));
        assert_eq!(a.svc_error.as_deref(), Some("docker down"));
        assert_eq!(a.stacks.as_ref().map(Vec::len), Some(0), "error still ends 'reading docker…'");
        a.apply(Msg::Stacks(Ok(vec![])));
        assert!(a.svc_error.is_none());
        a.pending.insert("s/gone".into(), (PendingKind::Starting, Instant::now()));
        a.apply(Msg::Stacks(Ok(vec![])));
        assert!(a.pending.is_empty(), "a service docker no longer lists is not pending");
        a.apply(Msg::Agents(HashMap::new()));
        assert!(a.need_reload);
        let snap = Snapshot { jobs: vec![], config_error: None, history_error: Some("locked".into()), at: Local::now() };
        a.apply(Msg::Snapshot(snap));
        assert_eq!(a.history_banner().as_deref(), Some("ERROR — locked"));
        a.history_open_error = Some("cannot open".into());
        let snap = Snapshot { jobs: vec![], config_error: None, history_error: None, at: Local::now() };
        a.apply(Msg::Snapshot(snap));
        assert_eq!(a.history_banner().as_deref(), Some("ERROR — cannot open"));
    }

    #[test]
    fn terminal_restored_on_panic() {
        install_panic_hook();
        install_panic_hook(); // installing twice must not chain or panic
        assert!(!restore_terminal(), "nothing to restore before setup, and no noise on a non-tty");
        TERMINAL_ACTIVE.store(true, Ordering::SeqCst);
        let mut out = Vec::new();
        assert!(restore_to(&mut out), "first restore does the work");
        let s = String::from_utf8_lossy(&out);
        assert!(s.contains("\x1b[?1000l"), "mouse capture disabled: {s:?}");
        assert!(s.contains("\x1b[?1049l"), "alternate screen left: {s:?}");
        assert!(!restore_to(&mut Vec::new()), "second restore is a no-op");
    }

    #[test]
    fn draw_stubs_render_titles_and_clear_hits() {
        let mut a = app();
        a.hits.push((Rect::new(0, 0, 1, 1), Hit::Confirm));
        let mut t = Terminal::new(ratatui::backend::TestBackend::new(20, 5)).unwrap();
        t.draw(|f| draw(f, &mut a)).unwrap();
        assert!(a.hits.is_empty());
        a.view = View::Services;
        t.draw(|f| draw(f, &mut a)).unwrap();
    }

    #[test]
    fn workers_wake_on_poke_and_stop_on_quit() {
        let ctl = Control::new();
        let (tx, rx) = mpsc::channel();
        spawn_worker(ctl.clone(), USAGE, move || {
            let _ = tx.send(());
        });
        rx.recv_timeout(Duration::from_secs(2)).expect("runs immediately");
        assert!(rx.recv_timeout(Duration::from_millis(200)).is_err(), "then sleeps");
        ctl.poke(USAGE, Duration::ZERO);
        rx.recv_timeout(Duration::from_secs(2)).expect("poke wakes it");
        ctl.quit();
    }

    #[test]
    fn periods() {
        assert_eq!(period(STACKS, true), Duration::from_secs(3));
        assert_eq!(period(STACKS, false), Duration::from_secs(15));
        assert_eq!(period(USAGE, true), Duration::from_secs(6));
        assert_eq!(period(USAGE, false), Duration::from_secs(30));
        assert_eq!(period(AGENTS, true), Duration::from_secs(30));
    }
}
