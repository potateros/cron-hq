//! The terminal UI: event loop, `App` state, global keys, mouse dispatch and the worker threads.
//! Views register their clickable regions in `App::hits` while drawing; everything else lives here.

mod canvas;
pub mod help;
pub mod jobs;
pub mod logview;
pub mod services;
pub mod theme;
#[cfg(test)]
pub mod testing;

use crate::config::load_config;
use crate::history::{open_history, History};
use crate::launchd::{launchctl_from_env, AgentState, SyncPlan};
use crate::paths::Paths;
use crate::snapshot::{fetch_agents, load_snapshot, Snapshot};
use crate::stacks::{
    is_up, load_stack_views, load_stacks, load_usage, totals, ConfigCache, Docker, RealDocker, Stack, StackView, Usage,
};
use chrono::{DateTime, Local, Timelike};
use crossterm::event::{
    self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton,
    MouseEvent, MouseEventKind,
};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;
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
    /// A tab in the title bar: switches to that view.
    Tab(View),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingKind {
    Starting,
    Stopping,
}

/// A start/stop that never shows up in docker is forgotten after this long.
pub const PENDING_TTL: Duration = Duration::from_secs(120);

/// Everything the event loop wakes for: terminal input and worker results share one channel.
pub enum LoopEvent {
    Input(Event),
    Msg(Msg),
    /// The input thread can no longer read the terminal: quit instead of hanging.
    Quit,
}

/// What worker threads and background actions report to the event loop.
pub enum Msg {
    Snapshot(Snapshot),
    Agents(HashMap<String, AgentState>),
    Stacks(Result<Vec<StackView>, String>),
    Usage(HashMap<String, Usage>),
    /// A compose start/stop worked; docker should be re-read now.
    ActionDone,
    /// A compose start/stop (or a worker) failed: the message, and the pending keys to forget.
    ActionFailed(String, Vec<String>),
    /// A worker thread panicked. Shown as the error banner; deliberately triggers no refresh, so a job that
    /// always panics cannot spin (ruling R23).
    WorkerFailed(String),
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
    /// The open log; replaced by a fresh one every time a log is opened.
    pub log: logview::LogState,
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
    /// Where `jobs.toml` and friends live; needed to plan a sync.
    pub paths: Option<Paths>,
    pub now: DateTime<Local>,
    pub refresh: Refresh,
    /// The snapshot should be rebuilt (new agent states arrived).
    pub need_reload: bool,
    pub dirty: bool,
    /// Where background actions report; unset in unit tests that do not look at results.
    pub tx: Option<Sender<LoopEvent>>,
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
            log: logview::LogState::default(),
            stacks: None,
            usage: HashMap::new(),
            agents: HashMap::new(),
            svc_error: None,
            action_error: None,
            history_open_error: None,
            history_error: None,
            pending: HashMap::new(),
            hits: Vec::new(),
            paths: None,
            now,
            refresh: Refresh::default(),
            need_reload: false,
            dirty: true,
            tx: None,
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
            Msg::WorkerFailed(e) => self.action_error = Some(e),
            Msg::ActionDone => self.refresh.stacks = true,
            Msg::ActionFailed(e, keys) => {
                self.action_error = Some(e);
                for k in &keys {
                    self.pending.remove(k);
                }
                self.refresh.stacks = true;
            }
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
        let other = match self.view {
            View::Jobs => View::Services,
            View::Services => View::Jobs,
        };
        self.show_view(other);
    }

    fn show_view(&mut self, view: View) {
        if self.view != view {
            self.view = view;
            self.refresh.stacks = true;
        }
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
        match ev.code {
            KeyCode::Tab | KeyCode::BackTab => {
                self.switch_view();
                return true;
            }
            KeyCode::Left => {
                self.show_view(View::Jobs);
                return true;
            }
            KeyCode::Right => {
                self.show_view(View::Services);
                return true;
            }
            _ => {}
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

    /// Keys inside the log view other than Esc.
    fn log_key(&mut self, ev: KeyEvent, _fx: &dyn Effects) {
        self.log.handle_key(ev);
    }

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
                if i == self.log.selected() {
                    return self.handle_key(key(KeyCode::Enter), fx);
                }
                self.log.select(i);
            }
            Some(Hit::Service(i)) => {
                if i == self.svc_selected {
                    return self.handle_key(key(KeyCode::Char('o')), fx);
                }
                self.svc_selected = i;
            }
            Some(Hit::Key(code)) => return self.handle_key(key(code), fx),
            Some(Hit::ToggleView) => return self.handle_key(key(KeyCode::Tab), fx),
            Some(Hit::Tab(v)) => self.show_view(v),
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

impl Default for RealEffects {
    fn default() -> Self {
        Self::new()
    }
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

// ─── drawing ─────────────────────────────────────────────────────────────────

pub fn draw(frame: &mut Frame, app: &mut App) {
    app.hits.clear();
    match (&app.mode, app.view) {
        (Mode::Log(id), _) => logview::draw_log(frame, &mut app.log, id, &mut app.hits),
        (Mode::Help, _) => help::draw_help(frame, app.view),
        (_, View::Jobs) => jobs::draw_jobs(frame, app),
        (_, View::Services) => services::draw_services(frame, app),
    }
}


// ─── terminal ────────────────────────────────────────────────────────────────

/// Clicks and wheel only (?1000) in SGR encoding (?1006). crossterm's EnableMouseCapture would also turn on
/// all-motion tracking (?1003), waking the loop on every pointer move.
const MOUSE_ON: &str = "\x1b[?1000h\x1b[?1006h";
const MOUSE_OFF: &str = "\x1b[?1006l\x1b[?1000l";

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
    let _ = out.write_all(MOUSE_OFF.as_bytes());
    let _ = crossterm::execute!(out, LeaveAlternateScreen, crossterm::cursor::Show);
    let _ = out.flush();
    true
}

fn setup_terminal() -> io::Result<Terminal<CrosstermBackend<io::Stdout>>> {
    enable_raw_mode()?;
    TERMINAL_ACTIVE.store(true, Ordering::SeqCst);
    let mut out = io::stdout();
    if let Err(e) = crossterm::execute!(out, EnterAlternateScreen).and_then(|_| out.write_all(MOUSE_ON.as_bytes())) {
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
            let main = std::thread::current().name() == Some("main");
            if main {
                restore_terminal();
                prev(info);
            } else if !TERMINAL_ACTIVE.load(Ordering::SeqCst) {
                prev(info);
            }
            // else: a worker panic while the TUI is up is caught and shown as an error line;
            // printing it would scribble over the screen.
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
fn panic_text(p: Box<dyn std::any::Any + Send>) -> String {
    p.downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| p.downcast_ref::<String>().cloned()).unwrap_or_else(|| "panic".into())
}

/// `start_now` false waits one period before the first run (the caller already did it).
fn spawn_worker(ctl: Arc<Control>, idx: usize, tx: Sender<LoopEvent>, start_now: bool, mut job: impl FnMut() + Send + 'static) {
    std::thread::spawn(move || {
        let mut last: Option<Instant> = if start_now { None } else { Some(Instant::now()) };
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
            if let Err(p) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(&mut job)) {
                let _ = tx.send(LoopEvent::Msg(Msg::WorkerFailed(format!("worker panicked: {}", panic_text(p)))));
            }
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

fn start_workers(ctl: &Arc<Control>, paths: &Paths, tx: &Sender<LoopEvent>, docker: Arc<dyn Docker>) {
    let usage: Arc<Mutex<HashMap<String, Usage>>> = Arc::default();

    let (p, t) = (paths.clone(), tx.clone());
    spawn_worker(ctl.clone(), AGENTS, tx.clone(), false, move || {
        let lc = launchctl_from_env(&p);
        if let Ok(jobs) = load_config(&p.config) {
            let _ = t.send(LoopEvent::Msg(Msg::Agents(fetch_agents(&jobs, lc.as_ref()))));
        }
    });

    let (t, d, u) = (tx.clone(), docker.clone(), usage.clone());
    let mut list = StackList::new(paths.config.clone());
    let mut cache = ConfigCache::new();
    spawn_worker(ctl.clone(), STACKS, tx.clone(), true, move || {
        let msg = list.get().and_then(|stacks| {
            let u = u.lock().unwrap_or_else(|e| e.into_inner()).clone();
            load_stack_views(&stacks, d.as_ref(), &u, &mut cache)
        });
        let _ = t.send(LoopEvent::Msg(Msg::Stacks(msg)));
    });

    let (t, c) = (tx.clone(), ctl.clone());
    let mut list = StackList::new(paths.config.clone());
    spawn_worker(ctl.clone(), USAGE, tx.clone(), true, move || {
        // docker down or stats failing: the ps-based rows still show state, just no CPU/MEM
        if !list.get().is_ok_and(|s| !s.is_empty()) {
            return;
        }
        if let Ok(map) = load_usage(docker.as_ref()) {
            *usage.lock().unwrap_or_else(|e| e.into_inner()) = map.clone();
            let _ = t.send(LoopEvent::Msg(Msg::Usage(map)));
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

/// Blocking terminal reads on their own thread, so input wakes the loop the instant it arrives.
fn spawn_input_thread(tx: Sender<LoopEvent>) {
    let _ = std::thread::Builder::new().name("input".into()).spawn(move || {
        pump_input(event::read, &tx);
    });
}

/// Forward terminal events until the channel closes; a read error ends the session with a quit.
fn pump_input(mut read: impl FnMut() -> io::Result<Event>, tx: &Sender<LoopEvent>) {
    loop {
        match read() {
            Ok(ev) => {
                if tx.send(LoopEvent::Input(ev)).is_err() {
                    break;
                }
            }
            Err(_) => {
                let _ = tx.send(LoopEvent::Quit);
                break;
            }
        }
    }
}

fn mode_sig(app: &App) -> (View, std::mem::Discriminant<Mode>, Option<String>) {
    let id = if let Mode::Log(id) = &app.mode { Some(id.clone()) } else { None };
    (app.view, std::mem::discriminant(&app.mode), id)
}

/// Handle one event. Returns false to quit.
fn handle_event(app: &mut App, ev: LoopEvent, fx: &dyn Effects, stale_mouse: &mut bool) -> bool {
    match ev {
        LoopEvent::Quit => return false,
        LoopEvent::Msg(m) => app.apply(m),
        LoopEvent::Input(Event::Key(k)) if k.kind != KeyEventKind::Release => {
            let before = mode_sig(app);
            let go = app.handle_key(k, fx);
            *stale_mouse |= mode_sig(app) != before;
            return go;
        }
        LoopEvent::Input(Event::Mouse(m)) => {
            // After a mode or view change the hits describe a screen that is gone.
            if *stale_mouse {
                return true;
            }
            let before = mode_sig(app);
            let go = app.handle_mouse(m, fx);
            *stale_mouse |= mode_sig(app) != before;
            return go;
        }
        LoopEvent::Input(Event::Resize(..)) => app.dirty = true,
        LoopEvent::Input(_) => {}
    }
    true
}

fn event_loop(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>, paths: &Paths) -> io::Result<()> {
    let fx = RealEffects::new();
    let ctl = Control::new();
    let (tx, rx): (Sender<LoopEvent>, Receiver<LoopEvent>) = mpsc::channel();
    let term = Arc::new(AtomicBool::new(false));
    for sig in [signal_hook::consts::SIGTERM, signal_hook::consts::SIGHUP] {
        let _ = signal_hook::flag::register(sig, term.clone());
    }

    let (mut history, open_err) = open_history(paths);
    let mut app = App::new(Local::now());
    app.history_open_error = open_err;
    app.paths = Some(paths.clone());
    app.tx = Some(tx.clone());
    // Agent states before the first frame, so jobs do not flash as unlinked.
    if let Ok(jobs) = load_config(&paths.config) {
        app.agents = fetch_agents(&jobs, launchctl_from_env(paths).as_ref());
    }
    start_workers(&ctl, paths, &tx, fx.docker());
    spawn_input_thread(tx.clone());

    let mut seen = [None; 5];
    let mut sent_view = app.view;
    let mut last_second = app.now.second();
    app.need_reload = true;

    let result = (|| -> io::Result<()> {
        loop {
            app.now = Local::now();
            // Following the log: one stat per wake (a second at the longest) while it is open.
            if app.refresh_log() {
                app.dirty = true;
            }
            if term.load(Ordering::Relaxed) {
                return Ok(());
            }
            let now_stamps = stamps(paths);
            if now_stamps != seen || app.need_reload {
                // A database that failed to open may be fixable; try again whenever something changed.
                if app.history_open_error.is_some() {
                    if let Ok(h) = History::open(&paths.db()) {
                        history = Some(h);
                        app.history_open_error = None;
                    }
                }
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

            // Sleep until input, a worker result or the next whole second; nothing else wakes us.
            let first = match rx.recv_timeout(until_next_second()) {
                Ok(ev) => Some(ev),
                Err(mpsc::RecvTimeoutError::Timeout) => None,
                Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(()),
            };
            let mut stale_mouse = false;
            for ev in first.into_iter().chain(std::iter::from_fn(|| rx.try_recv().ok())) {
                if !handle_event(&mut app, ev, &fx, &mut stale_mouse) {
                    return Ok(());
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

    use crate::tui::testing::Rec;

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
        a.snap = Some(crate::tui::testing::snapshot());
        assert!(a.handle_mouse(scroll(false), &fx));
        assert_eq!(a.selected, 1, "wheel down = Down x1 in the jobs list");
        assert!(a.handle_mouse(scroll(false), &fx));
        assert_eq!(a.selected, 2);
        assert!(a.handle_mouse(scroll(true), &fx));
        assert_eq!(a.selected, 1, "wheel up = Up x1");
        // The log's x3 scroll is asserted in logview::tests::wheel_scrolls_three_lines; here it must not leave the log.
        a.mode = Mode::Log("x".into());
        assert!(a.handle_mouse(scroll(false), &fx));
        assert!(matches!(a.mode, Mode::Log(_)));
        a.mode = Mode::Help;
        assert!(a.handle_mouse(scroll(true), &fx));
        assert!(matches!(a.mode, Mode::Help));
        assert_eq!(a.selected, 1, "help ignores the wheel");
    }

    #[test]
    fn input_read_error_quits_instead_of_exiting_silently() {
        let (tx, rx) = mpsc::channel();
        let mut n = 0;
        pump_input(
            || {
                n += 1;
                if n == 1 {
                    Ok(Event::Resize(1, 1))
                } else {
                    Err(io::Error::other("tty gone"))
                }
            },
            &tx,
        );
        assert!(matches!(rx.try_recv(), Ok(LoopEvent::Input(Event::Resize(1, 1)))));
        let ev = rx.try_recv().unwrap();
        assert!(matches!(ev, LoopEvent::Quit));
        let (mut a, fx) = (app(), Rec::default());
        assert!(!handle_event(&mut a, ev, &fx, &mut false));
    }

    #[test]
    fn apply_updates_state() {
        let mut a = app();
        a.apply(Msg::ActionFailed("boom".into(), vec![]));
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
        assert!(s.contains("\x1b[?1006l"), "{s:?}");
        assert!(!s.contains("\x1b[?1003"), "no all-motion tracking");
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
        let (etx, _erx) = mpsc::channel();
        spawn_worker(ctl.clone(), USAGE, etx, true, move || {
            let _ = tx.send(());
        });
        rx.recv_timeout(Duration::from_secs(2)).expect("runs immediately");
        assert!(rx.recv_timeout(Duration::from_millis(200)).is_err(), "then sleeps");
        ctl.poke(USAGE, Duration::ZERO);
        rx.recv_timeout(Duration::from_secs(2)).expect("poke wakes it");
        ctl.quit();
    }

    #[test]
    fn worker_failure_shows_the_error_and_never_pokes() {
        let mut a = app();
        a.pending.insert("s/x".into(), (PendingKind::Starting, Instant::now()));
        a.apply(Msg::WorkerFailed("worker panicked: boom".into()));
        assert_eq!(a.action_error.as_deref(), Some("worker panicked: boom"));
        assert!(!a.refresh.stacks && !a.refresh.agents, "no immediate re-run, so a panicking job cannot spin");
        assert_eq!(a.pending.len(), 1);
        a.apply(Msg::ActionFailed("lab: stop x — e".into(), vec!["s/x".into()]));
        assert!(a.refresh.stacks && a.pending.is_empty(), "compose failures still refresh");
    }

    #[test]
    fn worker_panic_becomes_an_error_message() {
        let ctl = Control::new();
        let (tx, rx) = mpsc::channel();
        spawn_worker(ctl.clone(), USAGE, tx, true, || panic!("kaboom"));
        match rx.recv_timeout(Duration::from_secs(2)) {
            Ok(LoopEvent::Msg(Msg::WorkerFailed(m))) => assert!(m.contains("kaboom"), "{m}"),
            _ => panic!("expected WorkerFailed"),
        }
        ctl.quit();
    }

    #[test]
    fn stale_mouse_events_are_dropped_after_a_mode_change() {
        let (mut a, fx) = (app(), Rec::default());
        a.hits = vec![(Rect::new(0, 0, 10, 1), Hit::Job(1))];
        let mut stale = false;
        let key_h = LoopEvent::Input(Event::Key(k(KeyCode::Char('h'))));
        assert!(handle_event(&mut a, key_h, &fx, &mut stale));
        assert!(stale && matches!(a.mode, Mode::Help));
        // A queued click would close help and then (if processed) act on the old hits; it must be skipped.
        let c = LoopEvent::Input(Event::Mouse(click(1, 0)));
        assert!(handle_event(&mut a, c, &fx, &mut stale));
        assert!(matches!(a.mode, Mode::Help));
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
