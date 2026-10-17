//! docker compose stacks listed under `[stacks.*]` in jobs.toml. Read by the TUI only.

use crate::config::ConfigError;
use crate::launchd::CmdOutput;
use crate::paths::expand_home;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant, UNIX_EPOCH};
use toml::Table;

/// How long polls wait for docker before giving up. Start/stop actions pass `None` (a pull can take minutes).
pub const POLL_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, PartialEq)]
pub struct Stack {
    pub name: String,
    pub dir: PathBuf,
    /// Absolute; empty = compose's default lookup in `dir`.
    pub files: Vec<PathBuf>,
    pub env_file: Option<PathBuf>,
}

const KEYS: [&str; 3] = ["dir", "files", "env_file"];

/// Lexical `path.join` like node's: drops `.` and folds `a/..`.
fn join_norm(base: &Path, rel: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in base.join(rel).components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                let last_normal = matches!(out.components().next_back(), Some(Component::Normal(_)));
                if last_normal {
                    out.pop();
                } else {
                    out.push("..");
                }
            }
            c => out.push(c.as_os_str()),
        }
    }
    out
}

pub fn parse_stacks(text: &str) -> Result<Vec<Stack>, ConfigError> {
    let doc: Table = text.parse().map_err(|e: toml::de::Error| ConfigError(format!("invalid TOML: {e}")))?;
    let Some(stacks) = doc.get("stacks") else { return Ok(Vec::new()) };
    let Some(stacks) = stacks.as_table() else { return Err(ConfigError("stacks must be a table".into())) };
    let mut out = Vec::new();
    for (name, raw) in stacks {
        let fail = |msg: &str| ConfigError(format!("stacks.{name}: {msg}"));
        let valid = !name.is_empty() && name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
        if !valid {
            return Err(fail(&format!("invalid stack name \"{name}\" (use a-z, 0-9 and -)")));
        }
        let Some(raw) = raw.as_table() else { return Err(fail("must be a table")) };
        for key in raw.keys() {
            if !KEYS.contains(&key.as_str()) {
                return Err(fail(&format!("unknown key {key}")));
            }
        }
        let Some(dir) = raw.get("dir").and_then(|d| d.as_str()) else { return Err(fail("dir is required")) };
        let dir = expand_home(dir);
        let abs = |p: &str| {
            let p = expand_home(p);
            if p.is_absolute() {
                p
            } else {
                join_norm(&dir, &p)
            }
        };
        let files = match raw.get("files") {
            None => Vec::new(),
            Some(v) => {
                let list: Option<Vec<&str>> = v.as_array().and_then(|a| a.iter().map(|f| f.as_str()).collect());
                match list {
                    Some(l) => l.into_iter().map(abs).collect(),
                    None => return Err(fail("files must be a list of paths")),
                }
            }
        };
        let env_file = match raw.get("env_file") {
            None => None,
            Some(v) => match v.as_str() {
                Some(s) => Some(abs(s)),
                None => return Err(fail("env_file must be a string")),
            },
        };
        out.push(Stack { name: name.clone(), dir: dir.clone(), files, env_file });
    }
    Ok(out)
}

pub fn load_stacks(path: &Path) -> Result<Vec<Stack>, ConfigError> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| ConfigError(format!("cannot read {}: {e}", path.display())))?;
    parse_stacks(&text)
}

// ─── docker ──────────────────────────────────────────────────────────────────

/// `timeout`: kill docker after this long; `None` waits as long as it takes.
/// Err is a failure to run docker at all (missing binary, timeout, spawn error).
pub trait Docker: Send + Sync {
    fn exec(&self, args: &[String], timeout: Option<Duration>) -> Result<CmdOutput, String>;
}

pub struct RealDocker;

impl Docker for RealDocker {
    fn exec(&self, args: &[String], timeout: Option<Duration>) -> Result<CmdOutput, String> {
        run_command("docker", args, timeout)
    }
}

fn drain<R: Read + Send + 'static>(mut r: R) -> mpsc::Receiver<String> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = r.read_to_end(&mut buf);
        let _ = tx.send(String::from_utf8_lossy(&buf).into_owned());
    });
    rx
}

fn run_command(program: &str, args: &[String], timeout: Option<Duration>) -> Result<CmdOutput, String> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                format!("{program} not found")
            } else {
                e.to_string()
            }
        })?;
    // Read on threads so a large output can't fill the pipe and stall the child.
    let out_rx = child.stdout.take().map(drain);
    let err_rx = child.stderr.take().map(drain);
    let deadline = timeout.map(|t| Instant::now() + t);
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) => {}
            Err(e) => return Err(e.to_string()),
        }
        if deadline.is_some_and(|d| Instant::now() >= d) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("{program} timed out after {}s", timeout.map_or(0, |t| t.as_secs())));
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    // A grandchild may still hold the pipe; don't wait for it forever.
    let grab = |rx: Option<mpsc::Receiver<String>>| rx.and_then(|r| r.recv_timeout(Duration::from_secs(2)).ok()).unwrap_or_default();
    Ok(CmdOutput { code: status.code().unwrap_or(-1), stdout: grab(out_rx), stderr: grab(err_rx) })
}

/// `docker compose` with the stack's project dir, env file and compose files.
pub fn compose_args(s: &Stack, rest: &[&str]) -> Vec<String> {
    let mut args: Vec<String> = vec!["compose".into(), "--project-directory".into(), s.dir.to_string_lossy().into_owned()];
    if let Some(e) = &s.env_file {
        args.push("--env-file".into());
        args.push(e.to_string_lossy().into_owned());
    }
    for f in &s.files {
        args.push("-f".into());
        args.push(f.to_string_lossy().into_owned());
    }
    args.extend(rest.iter().map(|r| r.to_string()));
    args
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceState {
    Running,
    Restarting,
    Paused,
    Exited,
    Created,
    Dead,
    /// In the compose file but no container exists.
    Missing,
}

impl ServiceState {
    /// Docker's `State` word. "removing" and an empty/absent word are stopped, not faults (ruling R18);
    /// "dead" and anything unrecognised count as dead so they show as a fault.
    fn from_docker(s: &str) -> ServiceState {
        match s {
            "running" => ServiceState::Running,
            "restarting" => ServiceState::Restarting,
            "paused" => ServiceState::Paused,
            "exited" | "removing" | "" => ServiceState::Exited,
            "created" => ServiceState::Created,
            _ => ServiceState::Dead,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ServiceView {
    pub stack: String,
    pub service: String,
    pub container: Option<String>,
    pub image: String,
    /// Published host ports, first = the web UI.
    pub ports: Vec<u16>,
    pub state: ServiceState,
    /// "", "healthy", "unhealthy", "starting".
    pub health: String,
    /// Docker's own wording, e.g. "Up 39 hours".
    pub status: String,
    pub exit_code: i32,
    /// Percent of one core, from docker stats.
    pub cpu: Option<f64>,
    /// Bytes.
    pub mem: Option<u64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StackView {
    pub stack: Stack,
    pub services: Vec<ServiceView>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Usage {
    pub cpu: f64,
    pub mem: u64,
}

/// "38.67MiB" → bytes.
pub fn parse_bytes(s: &str) -> Option<u64> {
    let s = s.trim();
    let split = s.find(|c: char| !(c.is_ascii_digit() || c == '.'))?;
    let (num, unit) = s.split_at(split);
    let unit = unit.trim_start();
    if num.is_empty() || unit.is_empty() || !unit.bytes().all(|b| b.is_ascii_alphabetic()) {
        return None;
    }
    let mult: f64 = match unit.to_ascii_uppercase().as_str() {
        "B" => 1.0,
        "KB" => 1e3,
        "MB" => 1e6,
        "GB" => 1e9,
        "TB" => 1e12,
        "KIB" => 1024.0,
        "MIB" => 1024f64.powi(2),
        "GIB" => 1024f64.powi(3),
        "TIB" => 1024f64.powi(4),
        _ => return None,
    };
    let n: f64 = num.parse().ok()?;
    Some((n * mult).round() as u64)
}

/// Compose prints either one JSON array or one object per line, depending on version.
fn parse_json_lines(stdout: &str) -> Result<Vec<Value>, String> {
    let t = stdout.trim();
    if t.is_empty() {
        return Ok(Vec::new());
    }
    if t.starts_with('[') {
        return match serde_json::from_str::<Value>(t) {
            Ok(Value::Array(a)) => Ok(a),
            Ok(_) => Err("unexpected compose output".into()),
            Err(e) => Err(e.to_string()),
        };
    }
    t.lines().filter(|l| !l.trim().is_empty()).map(|l| serde_json::from_str(l).map_err(|e| e.to_string())).collect()
}

/// A JSON field as text, like JS `String(x)` for the shapes docker emits; absent/null → "".
fn text(v: &Value, key: &str) -> String {
    match v.get(key) {
        Some(Value::String(s)) => s.clone(),
        None | Some(Value::Null) => String::new(),
        Some(o) => o.to_string(),
    }
}

/// Leading float like JS `parseFloat`: "12.50%" → 12.5, "--" → None.
fn parse_float_prefix(s: &str) -> Option<f64> {
    let s = s.trim_start();
    let end = s.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(s.len());
    s[..end].parse().ok()
}

/// NDJSON from `docker stats --no-stream --format '{{json .}}'` → usage by container name. Bad lines are skipped.
pub fn parse_stats(stdout: &str) -> HashMap<String, Usage> {
    let mut out = HashMap::new();
    for line in stdout.lines().filter(|l| !l.trim().is_empty()) {
        let Ok(row) = serde_json::from_str::<Value>(line) else { continue };
        let cpu = parse_float_prefix(&text(&row, "CPUPerc"));
        let mem_usage = text(&row, "MemUsage");
        let mem = parse_bytes(mem_usage.split('/').next().unwrap_or(""));
        if let (Some(name), Some(cpu), Some(mem)) = (row.get("Name").and_then(|n| n.as_str()), cpu, mem) {
            out.insert(name.to_string(), Usage { cpu, mem });
        }
    }
    out
}

#[derive(Debug, Clone, PartialEq)]
pub struct Declared {
    pub image: String,
    pub container_name: Option<String>,
    pub ports: Vec<u16>,
}

/// `compose config --format json` → declared services, in file order.
pub fn parse_compose_config(stdout: &str) -> Result<Vec<(String, Declared)>, String> {
    let doc: Value = serde_json::from_str(stdout).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    let Some(services) = doc.get("services").and_then(|s| s.as_object()) else { return Ok(out) };
    for (name, svc) in services {
        let mut ports: Vec<u16> = Vec::new();
        let mut seen = HashSet::new();
        for p in svc.get("ports").and_then(|p| p.as_array()).into_iter().flatten() {
            if p.get("protocol").and_then(|x| x.as_str()).is_some_and(|x| x != "tcp") {
                continue;
            }
            let n = match p.get("published") {
                Some(Value::String(s)) => s.trim().parse::<u64>().ok(),
                Some(Value::Number(n)) => n.as_u64(),
                _ => None,
            };
            if let Some(n) = n.filter(|&n| n > 0 && n <= u16::MAX as u64) {
                if seen.insert(n) {
                    ports.push(n as u16);
                }
            }
        }
        out.push((
            name.clone(),
            Declared {
                image: text(svc, "image"),
                container_name: svc.get("container_name").and_then(|c| c.as_str()).map(String::from),
                ports,
            },
        ));
    }
    Ok(out)
}

pub fn last_line(s: &str) -> &str {
    s.trim().lines().rfind(|l| !l.is_empty()).unwrap_or("")
}

/// `compose config` is slow-ish and only changes with the files, so cache it by their mtimes.
#[derive(Default)]
pub struct ConfigCache {
    entries: HashMap<String, (String, Vec<(String, Declared)>)>,
}

impl ConfigCache {
    pub fn new() -> Self {
        Self::default()
    }
}

fn files_key(s: &Stack) -> String {
    let mut files: Vec<PathBuf> = if s.files.is_empty() {
        ["compose.yaml", "compose.yml", "docker-compose.yml", "docker-compose.yaml"].iter().map(|f| s.dir.join(f)).collect()
    } else {
        s.files.clone()
    };
    files.push(s.env_file.clone().unwrap_or_else(|| s.dir.join(".env")));
    files
        .iter()
        .map(|f| {
            let mtime = std::fs::metadata(f)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map_or_else(|| "-".to_string(), |d| d.as_nanos().to_string());
            format!("{}@{mtime}", f.display())
        })
        .collect::<Vec<_>>()
        .join("|")
}

/// Why a stack could not be loaded: docker itself would not run, or compose reported a problem.
enum LoadErr {
    Exec(String),
    Compose(String),
}

impl LoadErr {
    fn into_message(self) -> String {
        match self {
            LoadErr::Exec(e) | LoadErr::Compose(e) => e,
        }
    }
}

pub fn load_stack_view(s: &Stack, docker: &dyn Docker, usage: &HashMap<String, Usage>, cache: &mut ConfigCache) -> StackView {
    match try_load(s, docker, usage, cache) {
        Ok(services) => StackView { stack: s.clone(), services, error: None },
        Err(e) => StackView { stack: s.clone(), services: Vec::new(), error: Some(e.into_message()) },
    }
}

/// All stacks. A docker that cannot run at all (missing binary, timeout) fails the whole load, as in the
/// TS `Promise.all`; compose complaints stay on their own stack.
pub fn load_stack_views(stacks: &[Stack], docker: &dyn Docker, usage: &HashMap<String, Usage>, cache: &mut ConfigCache) -> Result<Vec<StackView>, String> {
    stacks
        .iter()
        .map(|s| match try_load(s, docker, usage, cache) {
            Ok(services) => Ok(StackView { stack: s.clone(), services, error: None }),
            Err(LoadErr::Exec(e)) => Err(e),
            Err(LoadErr::Compose(e)) => Ok(StackView { stack: s.clone(), services: Vec::new(), error: Some(e) }),
        })
        .collect()
}

fn try_load(s: &Stack, docker: &dyn Docker, usage: &HashMap<String, Usage>, cache: &mut ConfigCache) -> Result<Vec<ServiceView>, LoadErr> {
    let key = files_key(s);
    if cache.entries.get(&s.name).map(|e| &e.0) != Some(&key) {
        let cfg = docker.exec(&compose_args(s, &["config", "--format", "json"]), Some(POLL_TIMEOUT)).map_err(LoadErr::Exec)?;
        if cfg.code != 0 {
            let l = last_line(&cfg.stderr);
            return Err(LoadErr::Compose(if l.is_empty() { format!("compose config exited {}", cfg.code) } else { l.to_string() }));
        }
        let declared = parse_compose_config(&cfg.stdout).map_err(LoadErr::Compose)?;
        cache.entries.insert(s.name.clone(), (key, declared));
    }
    let declared = &cache.entries[&s.name].1;
    let ps = docker.exec(&compose_args(s, &["ps", "-a", "--format", "json"]), Some(POLL_TIMEOUT)).map_err(LoadErr::Exec)?;
    if ps.code != 0 {
        let l = last_line(&ps.stderr);
        return Err(LoadErr::Compose(if l.is_empty() { format!("compose ps exited {}", ps.code) } else { l.to_string() }));
    }
    let live: HashMap<String, Value> = parse_json_lines(&ps.stdout).map_err(LoadErr::Compose)?.into_iter().map(|r| (text(&r, "Service"), r)).collect();
    Ok(declared
        .iter()
        .map(|(service, d)| {
            let r = live.get(service);
            let container = r.map(|r| text(r, "Name"));
            let u = container.as_ref().and_then(|c| usage.get(c));
            ServiceView {
                stack: s.name.clone(),
                service: service.clone(),
                container,
                image: d.image.clone(),
                ports: d.ports.clone(),
                state: r.map_or(ServiceState::Missing, |r| ServiceState::from_docker(&text(r, "State"))),
                health: r.map(|r| text(r, "Health")).unwrap_or_default(),
                status: r.map_or_else(|| "no container".to_string(), |r| text(r, "Status")),
                exit_code: r.and_then(|r| r.get("ExitCode")).and_then(|c| c.as_i64()).unwrap_or(0) as i32,
                cpu: u.map(|u| u.cpu),
                mem: u.map(|u| u.mem),
            }
        })
        .collect())
}

pub fn load_usage(docker: &dyn Docker) -> Result<HashMap<String, Usage>, String> {
    let args: Vec<String> = ["stats", "--no-stream", "--format", "{{json .}}"].iter().map(|s| s.to_string()).collect();
    let r = docker.exec(&args, Some(POLL_TIMEOUT))?;
    if r.code != 0 {
        let l = last_line(&r.stderr);
        return Err(if l.is_empty() { format!("docker stats exited {}", r.code) } else { l.to_string() });
    }
    Ok(parse_stats(&r.stdout))
}

pub fn is_up(v: &ServiceView) -> bool {
    matches!(v.state, ServiceState::Running | ServiceState::Restarting)
}

/// Running but unhealthy, restarting, or exited with an error.
pub fn is_service_fault(v: &ServiceView) -> bool {
    if matches!(v.state, ServiceState::Restarting | ServiceState::Dead) || v.health == "unhealthy" {
        return true;
    }
    // 137/143 = stopped by us
    v.state == ServiceState::Exited && v.exit_code != 0 && v.exit_code != 137 && v.exit_code != 143
}

pub fn service_url(v: &ServiceView) -> Option<String> {
    v.ports.first().map(|p| format!("http://localhost:{p}"))
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Totals {
    pub up: usize,
    pub all: usize,
    pub cpu: f64,
    pub mem: u64,
}

pub fn totals(views: &[ServiceView]) -> Totals {
    let mut t = Totals { up: 0, all: views.len(), cpu: 0.0, mem: 0 };
    for v in views {
        if is_up(v) {
            t.up += 1;
        }
        t.cpu += v.cpu.unwrap_or(0.0);
        t.mem += v.mem.unwrap_or(0);
    }
    t
}

pub fn format_bytes(n: u64) -> String {
    const K: u64 = 1024;
    let f = n as f64;
    if n >= K.pow(3) {
        format!("{:.1}G", f / K.pow(3) as f64)
    } else if n >= K.pow(2) {
        format!("{}M", (f / K.pow(2) as f64).round())
    } else if n >= K {
        format!("{}K", (f / K as f64).round())
    } else {
        format!("{n}B")
    }
}

/// Docker's "Up 39 hours (healthy)" / "Up About an hour" → "39h" / "1h".
pub fn short_uptime(status: &str) -> String {
    let mut s = status.trim();
    if s.len() >= 3 && s.is_char_boundary(2) && s[..2].eq_ignore_ascii_case("up") && s[2..].starts_with(char::is_whitespace) {
        s = s[2..].trim_start();
    }
    if s.ends_with(')') {
        if let Some(i) = s.find('(') {
            s = s[..i].trim_end();
        }
    }
    let s = s.trim();
    if s.eq_ignore_ascii_case("less than a second") {
        return "<1s".to_string();
    }
    let lower = s.to_ascii_lowercase();
    let words: Vec<&str> = lower.split_whitespace().collect();
    let (n, unit) = match words.as_slice() {
        [n, unit] if !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) => (n.to_string(), *unit),
        [a, unit] if matches!(*a, "a" | "an") => ("1".to_string(), *unit),
        ["about", a, unit] if matches!(*a, "a" | "an") => ("1".to_string(), *unit),
        _ => return s.to_string(),
    };
    let unit = unit.strip_suffix('s').unwrap_or(unit);
    let short = match unit {
        "second" => "s",
        "minute" => "m",
        "hour" => "h",
        "day" => "d",
        "week" => "w",
        "month" => "mo",
        "year" => "y",
        _ => return s.to_string(),
    };
    format!("{n}{short}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::time::SystemTime;

    fn home() -> PathBuf {
        expand_home("~")
    }

    #[test]
    fn parse_stacks_reads_dir_files_env() {
        let s = parse_stacks("[stacks.terminus]\ndir = \"~/x\"\nfiles = [\"a/compose.yml\", \"/abs/o.yml\"]\nenv_file = \"a/.env\"\n").unwrap();
        assert_eq!(
            s,
            vec![Stack {
                name: "terminus".into(),
                dir: home().join("x"),
                files: vec![home().join("x/a/compose.yml"), PathBuf::from("/abs/o.yml")],
                env_file: Some(home().join("x/a/.env")),
            }]
        );
    }

    #[test]
    fn no_stacks_table_is_fine() {
        assert!(parse_stacks("[jobs.a]\ncommand = \"x\"\nevery = \"5m\"\n").unwrap().is_empty());
    }

    #[test]
    fn rejects_unknown_keys_and_missing_dir() {
        assert_eq!(parse_stacks("[stacks.a]\ndir = \"/x\"\nport = 1\n").unwrap_err().0, "stacks.a: unknown key port");
        assert_eq!(parse_stacks("[stacks.a]\n").unwrap_err().0, "stacks.a: dir is required");
        assert!(parse_stacks("[stacks.A]\ndir = \"/x\"\n").unwrap_err().0.starts_with("stacks.A: invalid stack name"));
        assert_eq!(parse_stacks("[stacks.a]\ndir = \"/x\"\nfiles = [1]\n").unwrap_err().0, "stacks.a: files must be a list of paths");
        assert_eq!(parse_stacks("[stacks.a]\ndir = \"/x\"\nenv_file = 1\n").unwrap_err().0, "stacks.a: env_file must be a string");
        assert!(parse_stacks("[stacks.a").unwrap_err().0.starts_with("invalid TOML: "));
    }

    #[test]
    fn relative_paths_are_normalised() {
        let s = parse_stacks("[stacks.a]\ndir = \"/r\"\nfiles = [\"./x/../b.yml\"]\n").unwrap();
        assert_eq!(s[0].files, vec![PathBuf::from("/r/b.yml")]);
    }

    #[test]
    fn stacks_keep_file_order_and_load_reports_unreadable() {
        let s = parse_stacks("[stacks.zed]\ndir = \"/a\"\n[stacks.alpha]\ndir = \"/b\"\n").unwrap();
        assert_eq!(s.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), ["zed", "alpha"]);
        let e = load_stacks(Path::new("/nonexistent/jobs.toml")).unwrap_err();
        assert!(e.0.starts_with("cannot read /nonexistent/jobs.toml: "), "{}", e.0);
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("jobs.toml");
        std::fs::write(&p, "[stacks.a]\ndir = \"/a\"\n").unwrap();
        assert_eq!(load_stacks(&p).unwrap().len(), 1);
    }

    #[test]
    fn parse_bytes_units() {
        assert_eq!(parse_bytes("864KiB"), Some(864 * 1024));
        assert_eq!(parse_bytes("38.5MiB"), Some((38.5 * 1024.0 * 1024.0) as u64));
        assert_eq!(parse_bytes("1.2GB"), Some(1_200_000_000));
        assert_eq!(parse_bytes("nope"), None);
        assert_eq!(parse_bytes("--"), None);
        assert_eq!(parse_bytes(""), None);
    }

    #[test]
    fn parse_stats_rows() {
        let out = parse_stats(
            "{\"CPUPerc\":\"12.50%\",\"MemUsage\":\"38.67MiB / 256MiB\",\"Name\":\"jellyfin\"}\n{\"CPUPerc\":\"--\",\"MemUsage\":\"-- / --\",\"Name\":\"dead\"}\nnot json\n",
        );
        assert_eq!(out["jellyfin"], Usage { cpu: 12.5, mem: (38.67 * 1024.0 * 1024.0f64).round() as u64 });
        assert!(!out.contains_key("dead"));
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn parse_compose_config_keeps_tcp_ports_in_order() {
        let d = parse_compose_config(
            &serde_json::json!({ "services": {
                "jellyfin": { "image": "jellyfin/jellyfin", "container_name": "jellyfin", "ports": [
                    { "target": 8096, "published": "8096", "protocol": "tcp" },
                    { "target": 1900, "published": "1900", "protocol": "udp" },
                    { "target": 8920, "published": "8920" },
                    { "target": 8096, "published": 8096, "protocol": "tcp" },
                ] },
                "cloudflared": { "image": "cloudflare/cloudflared", "ports": null },
            } })
            .to_string(),
        )
        .unwrap();
        assert_eq!(d.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(), ["jellyfin", "cloudflared"]);
        assert_eq!(d[0].1.ports, vec![8096, 8920]);
        assert_eq!(d[0].1.container_name.as_deref(), Some("jellyfin"));
        assert!(d[1].1.ports.is_empty());
        assert!(parse_compose_config("garbage").is_err());
    }

    #[test]
    fn short_uptime_cases() {
        assert_eq!(short_uptime("Up 39 hours"), "39h");
        assert_eq!(short_uptime("Up 39 hours (healthy)"), "39h");
        assert_eq!(short_uptime("Up About an hour"), "1h");
        assert_eq!(short_uptime("Up Less than a second"), "<1s");
        assert_eq!(short_uptime("Up 2 days"), "2d");
        assert_eq!(short_uptime("Up 3 months"), "3mo");
        assert_eq!(short_uptime("Up 5 weeks"), "5w");
        assert_eq!(short_uptime("Up a minute"), "1m");
        assert_eq!(short_uptime("Up 1 fortnight"), "1 fortnight");
        assert_eq!(short_uptime("Exited (1) 5 minutes ago"), "Exited (1) 5 minutes ago");
    }

    #[test]
    fn format_bytes_units() {
        assert_eq!(format_bytes(512), "512B");
        assert_eq!(format_bytes(2048), "2K");
        assert_eq!(format_bytes(5 * 1024 * 1024), "5M");
        assert_eq!(format_bytes(3 * 1024 * 1024 * 1024 / 2), "1.5G");
    }

    // ─── loadStackView ───

    struct FakeDocker {
        calls: Mutex<Vec<Vec<String>>>,
        config: String,
        ps: String,
        fail: Option<String>,
    }

    impl FakeDocker {
        fn new(config: String, ps: String) -> Self {
            FakeDocker { calls: Mutex::new(Vec::new()), config, ps, fail: None }
        }
        fn calls(&self) -> Vec<Vec<String>> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl Docker for FakeDocker {
        fn exec(&self, args: &[String], timeout: Option<Duration>) -> Result<CmdOutput, String> {
            self.calls.lock().unwrap().push(args.to_vec());
            if let Some(f) = &self.fail {
                return Err(f.clone());
            }
            assert_eq!(timeout, Some(Duration::from_secs(15)));
            let has = |w: &str| args.iter().any(|a| a == w);
            let ok = |stdout: &str| Ok(CmdOutput { code: 0, stdout: stdout.to_string(), stderr: String::new() });
            if has("config") {
                ok(&self.config)
            } else if has("ps") {
                ok(&self.ps)
            } else {
                Ok(CmdOutput { code: 1, stdout: String::new(), stderr: "unexpected".into() })
            }
        }
    }

    fn lab() -> Stack {
        Stack { name: "lab".into(), dir: "/nonexistent-lab".into(), files: vec![], env_file: None }
    }

    fn config_json() -> String {
        serde_json::json!({ "services": {
            "jellyfin": { "image": "jellyfin/jellyfin", "ports": [{ "target": 8096, "published": "8096", "protocol": "tcp" }] },
            "radarr": { "image": "radarr", "ports": [{ "target": 7878, "published": "7878" }] },
            "sonarr": { "image": "sonarr", "ports": [] },
        } })
        .to_string()
    }

    fn ps_rows() -> Vec<Value> {
        vec![
            serde_json::json!({ "Service": "jellyfin", "Name": "jellyfin", "State": "running", "Health": "", "Status": "Up 2 hours", "ExitCode": 0 }),
            serde_json::json!({ "Service": "radarr", "Name": "radarr", "State": "exited", "Health": "", "Status": "Exited (1) 5 minutes ago", "ExitCode": 1 }),
        ]
    }

    fn ndjson(rows: &[Value]) -> String {
        rows.iter().map(|r| r.to_string()).collect::<Vec<_>>().join("\n")
    }

    #[test]
    fn merges_declared_services_containers_and_usage() {
        let docker = FakeDocker::new(config_json(), ndjson(&ps_rows()));
        let usage = HashMap::from([("jellyfin".to_string(), Usage { cpu: 80.0, mem: 2 * 1024 * 1024 * 1024 })]);
        let v = load_stack_view(&lab(), &docker, &usage, &mut ConfigCache::new());
        assert_eq!(v.error, None);
        let [jf, ra, so] = &v.services[..] else { panic!("{:?}", v.services) };
        assert_eq!((jf.service.as_str(), jf.state, jf.cpu, jf.ports.clone()), ("jellyfin", ServiceState::Running, Some(80.0), vec![8096]));
        assert_eq!(service_url(jf).as_deref(), Some("http://localhost:8096"));
        assert_eq!((ra.state, ra.exit_code), (ServiceState::Exited, 1));
        assert!(is_service_fault(ra));
        assert_eq!((so.state, so.container.clone(), so.cpu, so.status.as_str()), (ServiceState::Missing, None, None, "no container"));
        assert_eq!(service_url(so), None);
        assert_eq!(totals(&v.services), Totals { up: 1, all: 3, cpu: 80.0, mem: 2 * 1024 * 1024 * 1024 });
        assert_eq!(docker.calls()[0], ["compose", "--project-directory", "/nonexistent-lab", "config", "--format", "json"]);
        assert_eq!(docker.calls()[1], ["compose", "--project-directory", "/nonexistent-lab", "ps", "-a", "--format", "json"]);
    }

    #[test]
    fn ps_output_as_json_array_or_ndjson() {
        let rows = ps_rows();
        let as_array = FakeDocker::new(config_json(), serde_json::to_string(&rows).unwrap());
        let as_lines = FakeDocker::new(config_json(), ndjson(&rows) + "\n");
        let a = load_stack_view(&lab(), &as_array, &HashMap::new(), &mut ConfigCache::new());
        let b = load_stack_view(&lab(), &as_lines, &HashMap::new(), &mut ConfigCache::new());
        assert_eq!(a.error, None);
        assert_eq!(a.services.len(), 3);
        assert_eq!(a, b);
    }

    #[test]
    fn compose_error_is_reported_on_the_stack() {
        struct Failing;
        impl Docker for Failing {
            fn exec(&self, _: &[String], _: Option<Duration>) -> Result<CmdOutput, String> {
                Ok(CmdOutput { code: 1, stdout: String::new(), stderr: "warn\nno such file\n".into() })
            }
        }
        let v = load_stack_view(&lab(), &Failing, &HashMap::new(), &mut ConfigCache::new());
        assert_eq!(v.error.as_deref(), Some("no such file"));
        assert!(v.services.is_empty());
    }

    #[test]
    fn silent_failure_names_the_exit_code_and_exec_errors_pass_through() {
        struct Silent;
        impl Docker for Silent {
            fn exec(&self, _: &[String], _: Option<Duration>) -> Result<CmdOutput, String> {
                Ok(CmdOutput { code: 7, stdout: String::new(), stderr: String::new() })
            }
        }
        let v = load_stack_view(&lab(), &Silent, &HashMap::new(), &mut ConfigCache::new());
        assert_eq!(v.error.as_deref(), Some("compose config exited 7"));
        let mut d = FakeDocker::new(String::new(), String::new());
        d.fail = Some("docker not found".into());
        let v = load_stack_view(&lab(), &d, &HashMap::new(), &mut ConfigCache::new());
        assert_eq!(v.error.as_deref(), Some("docker not found"));
        assert_eq!(load_usage(&d).unwrap_err(), "docker not found");
    }

    #[test]
    fn config_is_cached_until_a_compose_file_changes() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("compose.yml");
        std::fs::write(&file, "x").unwrap();
        let stack = Stack { name: "c".into(), dir: dir.path().into(), files: vec![file.clone()], env_file: None };
        let docker = FakeDocker::new(config_json(), ndjson(&ps_rows()));
        let mut cache = ConfigCache::new();
        let configs = |d: &FakeDocker| d.calls().iter().filter(|c| c.iter().any(|a| a == "config")).count();
        load_stack_view(&stack, &docker, &HashMap::new(), &mut cache);
        load_stack_view(&stack, &docker, &HashMap::new(), &mut cache);
        assert_eq!(configs(&docker), 1);
        assert_eq!(docker.calls().len(), 3); // config once, ps twice
        let f = std::fs::OpenOptions::new().write(true).open(&file).unwrap();
        f.set_modified(SystemTime::now() + Duration::from_secs(60)).unwrap();
        load_stack_view(&stack, &docker, &HashMap::new(), &mut cache);
        assert_eq!(configs(&docker), 2);
    }

    #[test]
    fn default_key_covers_default_names_and_dot_env() {
        let dir = tempfile::tempdir().unwrap();
        let stack = Stack { name: "k".into(), dir: dir.path().into(), files: vec![], env_file: None };
        let before = files_key(&stack);
        assert!(before.contains("docker-compose.yaml@-") && before.contains(".env@-"));
        std::fs::write(dir.path().join(".env"), "A=1").unwrap();
        assert_ne!(before, files_key(&stack));
    }

    #[test]
    fn compose_args_adds_env_file_and_compose_files() {
        let s = Stack { name: "t".into(), dir: "/r".into(), files: vec!["/r/a.yml".into(), "/r/b.yml".into()], env_file: Some("/r/.env".into()) };
        assert_eq!(
            compose_args(&s, &["ps"]),
            ["compose", "--project-directory", "/r", "--env-file", "/r/.env", "-f", "/r/a.yml", "-f", "/r/b.yml", "ps"]
        );
    }

    #[test]
    fn load_usage_parses_stats() {
        struct Stats;
        impl Docker for Stats {
            fn exec(&self, args: &[String], timeout: Option<Duration>) -> Result<CmdOutput, String> {
                assert_eq!(args[0], "stats");
                assert_eq!(timeout, Some(Duration::from_secs(15)));
                Ok(CmdOutput { code: 0, stdout: "{\"CPUPerc\":\"1.0%\",\"MemUsage\":\"1KiB / 2KiB\",\"Name\":\"a\"}".into(), stderr: String::new() })
            }
        }
        assert_eq!(load_usage(&Stats).unwrap()["a"], Usage { cpu: 1.0, mem: 1024 });
    }

    #[test]
    fn removing_and_empty_state_are_stopped_not_faults() {
        assert_eq!(ServiceState::from_docker("removing"), ServiceState::Exited);
        assert_eq!(ServiceState::from_docker(""), ServiceState::Exited);
        assert_eq!(ServiceState::from_docker("dead"), ServiceState::Dead);
        assert_eq!(ServiceState::from_docker("bogus"), ServiceState::Dead);
        let docker = FakeDocker::new(config_json(), ndjson(&[serde_json::json!({ "Service": "jellyfin", "Name": "j", "State": "removing", "ExitCode": 0 })]));
        let v = load_stack_view(&lab(), &docker, &HashMap::new(), &mut ConfigCache::new());
        assert!(!is_service_fault(&v.services[0]) && !is_up(&v.services[0]));
    }

    #[test]
    fn exec_failure_fails_the_whole_load_but_compose_errors_stay_on_the_stack() {
        let mut d = FakeDocker::new(String::new(), String::new());
        d.fail = Some("docker not found".into());
        let r = load_stack_views(&[lab()], &d, &HashMap::new(), &mut ConfigCache::new());
        assert_eq!(r.unwrap_err(), "docker not found");
        struct Failing;
        impl Docker for Failing {
            fn exec(&self, _: &[String], _: Option<Duration>) -> Result<CmdOutput, String> {
                Ok(CmdOutput { code: 1, stdout: String::new(), stderr: "no such file".into() })
            }
        }
        let v = load_stack_views(&[lab()], &Failing, &HashMap::new(), &mut ConfigCache::new()).unwrap();
        assert_eq!(v[0].error.as_deref(), Some("no such file"));
    }

    #[test]
    fn fault_rules() {
        let mut v = ServiceView {
            stack: "s".into(), service: "a".into(), container: None, image: String::new(), ports: vec![],
            state: ServiceState::Exited, health: String::new(), status: String::new(), exit_code: 137, cpu: None, mem: None,
        };
        assert!(!is_service_fault(&v));
        v.exit_code = 143;
        assert!(!is_service_fault(&v));
        v.exit_code = 2;
        assert!(is_service_fault(&v));
        v.state = ServiceState::Running;
        v.health = "unhealthy".into();
        assert!(is_service_fault(&v));
        v.health.clear();
        assert!(!is_service_fault(&v));
        v.state = ServiceState::Restarting;
        assert!(is_service_fault(&v) && is_up(&v));
    }

    // ─── real process handling (no docker) ───

    fn sv(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn missing_binary_maps_to_not_found() {
        assert_eq!(run_command("hq-no-such-binary-xyz", &[], None).unwrap_err(), "hq-no-such-binary-xyz not found");
    }

    #[test]
    fn run_command_captures_output_and_exit_code() {
        let r = run_command("sh", &sv(&["-c", "echo out; echo err >&2; exit 3"]), Some(Duration::from_secs(5))).unwrap();
        assert_eq!((r.code, r.stdout.as_str(), r.stderr.as_str()), (3, "out\n", "err\n"));
    }

    #[test]
    fn run_command_handles_large_output() {
        let r = run_command("sh", &sv(&["-c", "head -c 1000000 /dev/zero | tr '\\0' x"]), Some(Duration::from_secs(10))).unwrap();
        assert_eq!(r.stdout.len(), 1_000_000);
    }

    #[test]
    fn run_command_kills_at_the_deadline() {
        let t = Instant::now();
        let e = run_command("sleep", &sv(&["30"]), Some(Duration::from_millis(200))).unwrap_err();
        assert!(e.contains("timed out"), "{e}");
        assert!(t.elapsed() < Duration::from_secs(5));
    }
}
