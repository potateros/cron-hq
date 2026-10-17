use crate::config::Job;
use crate::history::{History, Status, Trigger};
use crate::log::{append_log, rotate_if_needed, TailBuffer};
use crate::paths::Paths;
use crate::schedule::in_quiet;
use crate::state::read_paused;
use chrono::{DateTime, Local};
use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
use signal_hook::SigId;
use std::ffi::OsString;
use std::io::Read;
use std::path::Path;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub const TIMEOUT_EXIT: i32 = 124;

/// How long to wait for stdout/stderr to close after the shell exits (a stray grandchild may hold them).
const DRAIN: Duration = Duration::from_secs(2);
/// How often the receive loop wakes to check for forwarded signals.
const POLL: Duration = Duration::from_millis(100);
const MAX_LOG_BYTES: u64 = 1_048_576;
const PRUNE_AGE_MS: i64 = 90 * 86_400_000;

pub struct RunOpts {
    pub manual: bool,
    pub kill_grace: Duration,
    pub now: fn() -> DateTime<Local>,
}

pub fn format_duration(ms: i64) -> String {
    let s = (ms.max(0) + 500) / 1000;
    if s < 60 {
        format!("{s}s")
    } else {
        format!("{}m{:02}s", s / 60, s % 60)
    }
}

/// launchd starts us with PATH=/usr/bin:/bin, and `zsh -lc` only adds what ~/.zprofile sets (Homebrew).
/// User tool dirs are usually added in ~/.zshrc, which non-interactive shells skip, so add the common ones.
pub fn job_env() -> Vec<(OsString, OsString)> {
    let home = std::env::var_os("HOME").unwrap_or_default();
    job_env_from(std::env::vars_os(), Path::new(&home))
}

/// Takes the variables as an argument so tests never touch the real process environment.
/// Uses OsStrings throughout: `std::env::vars()` panics on non-Unicode values.
fn job_env_from(vars: impl Iterator<Item = (OsString, OsString)>, home: &Path) -> Vec<(OsString, OsString)> {
    let mut path = None;
    let mut env: Vec<(OsString, OsString)> = Vec::new();
    for (k, v) in vars {
        if k == "PATH" {
            path = Some(v).filter(|p| !p.is_empty());
        } else {
            env.push((k, v));
        }
    }
    let mut out = OsString::new();
    for dir in [home.join(".local/bin"), home.join(".bun/bin")] {
        out.push(dir);
        out.push(":");
    }
    out.push("/opt/homebrew/bin:/usr/local/bin:");
    out.push(path.unwrap_or_else(|| OsString::from("/usr/bin:/bin")));
    env.push((OsString::from("PATH"), out));
    env
}

fn is_alive(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }
    // SAFETY: signal 0 only checks existence.
    if unsafe { libc::kill(pid, 0) } == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

fn read_pid(path: &std::path::Path) -> Option<i32> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

enum Lock {
    Held,
    Busy,
    /// The lock could not be created for a reason other than a live holder; run unlocked.
    Unavailable(String),
}

fn acquire_lock(paths: &Paths, id: &str) -> Lock {
    use std::io::Write;
    let path = paths.lock(id);
    if let Some(dir) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(dir) {
            return Lock::Unavailable(e.to_string());
        }
    }
    for _ in 0..2 {
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut f) => {
                return match f.write_all(std::process::id().to_string().as_bytes()) {
                    Ok(()) => Lock::Held,
                    Err(e) => {
                        let _ = std::fs::remove_file(&path);
                        Lock::Unavailable(e.to_string())
                    }
                };
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                let mut pid = read_pid(&path);
                if pid.is_none() {
                    // The holder may be between create and write; look again before calling it stale.
                    std::thread::sleep(Duration::from_millis(50));
                    pid = read_pid(&path);
                }
                if pid.is_some_and(is_alive) {
                    return Lock::Busy;
                }
                let _ = std::fs::remove_file(&path); // stale: holder is gone
            }
            Err(e) => return Lock::Unavailable(e.to_string()),
        }
    }
    Lock::Busy
}

fn release_lock(paths: &Paths, id: &str) {
    let _ = std::fs::remove_file(paths.lock(id));
}

enum Event {
    Out(Vec<u8>),
    Eof,
    /// Exit code; 1 when killed by a signal (like the TS runner) or when wait failed.
    Exit(i32),
}

fn spawn_reader<R: Read + Send + 'static>(mut r: R, tx: mpsc::Sender<Event>) {
    std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match r.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if tx.send(Event::Out(buf[..n].to_vec())).is_err() {
                        return;
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => break,
            }
        }
        let _ = tx.send(Event::Eof);
    });
}

/// Forwarded-signal flags. Dropping this unregisters our actions, but signal-hook leaves its handler
/// installed for the rest of the process (it does not restore the default disposition). That is fine:
/// `hq run` is one-shot, and the TUI spawns `hq run` as a separate process.
struct Forwarded {
    flags: Vec<(&'static str, Arc<AtomicBool>)>,
    ids: Vec<SigId>,
}

impl Forwarded {
    fn install() -> Forwarded {
        let mut f = Forwarded { flags: Vec::new(), ids: Vec::new() };
        for (name, sig) in [("SIGTERM", SIGTERM), ("SIGINT", SIGINT), ("SIGHUP", SIGHUP)] {
            let flag = Arc::new(AtomicBool::new(false));
            if let Ok(id) = signal_hook::flag::register(sig, Arc::clone(&flag)) {
                f.ids.push(id);
                f.flags.push((name, flag));
            }
        }
        f
    }
    fn received(&self) -> Option<&'static str> {
        self.flags.iter().find(|(_, f)| f.load(Ordering::Relaxed)).map(|(n, _)| *n)
    }
}

impl Drop for Forwarded {
    fn drop(&mut self) {
        for id in self.ids.drain(..) {
            signal_hook::low_level::unregister(id);
        }
    }
}

struct Outcome {
    code: Option<i32>,
    timed_out: bool,
}

fn killpg(pgid: i32, sig: i32) {
    // SAFETY: plain syscall; a stale pgid just yields ESRCH.
    unsafe {
        libc::killpg(pgid, sig);
    }
}

/// Runs the command in its own process group, appending output to the job log and `tail`.
fn execute(job: &Job, paths: &Paths, tail: &mut TailBuffer, opts: &RunOpts, killed_by: &mut Option<&'static str>) -> Outcome {
    let output = |tail: &mut TailBuffer, chunk: &[u8]| {
        let _ = append_log(paths, &job.id, chunk);
        tail.push(chunk);
    };
    let forwarded = Forwarded::install();
    let mut cmd = Command::new("/bin/zsh");
    cmd.args(["-lc", &job.command])
        .current_dir(&job.cwd)
        .envs(job_env())
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            output(tail, format!("hq: failed to start: {e}\n").as_bytes());
            return Outcome { code: Some(1), timed_out: false };
        }
    };
    let pgid = child.id() as i32;
    let (tx, rx) = mpsc::channel();
    if let Some(o) = child.stdout.take() {
        spawn_reader(o, tx.clone());
    } else {
        let _ = tx.send(Event::Eof);
    }
    if let Some(e) = child.stderr.take() {
        spawn_reader(e, tx.clone());
    } else {
        let _ = tx.send(Event::Eof);
    }
    let wait_tx = tx.clone();
    std::thread::spawn(move || {
        let code = child.wait().ok().and_then(|s| s.code()).unwrap_or(1);
        let _ = wait_tx.send(Event::Exit(code));
    });
    drop(tx);

    let started = Instant::now();
    // An absurd timeout_sec would overflow Instant and panic: clamp to a week instead.
    let deadline = started
        .checked_add(Duration::from_secs(job.timeout_sec))
        .unwrap_or_else(|| started + Duration::from_secs(7 * 24 * 3600));
    let mut term_at: Option<Instant> = None;
    let mut kill_sent = false;
    let mut timed_out = false;
    let mut exit: Option<i32> = None;
    let mut drain_until: Option<Instant> = None;
    let mut eofs = 0;
    let mut disconnected = false;

    loop {
        let now = Instant::now();
        // Sleep no longer than the next thing we need to act on.
        let mut wait = POLL;
        let mut consider = |t: Instant| wait = wait.min(t.saturating_duration_since(now));
        if exit.is_none() && !timed_out {
            consider(deadline);
        }
        if let (Some(t), false) = (term_at, kill_sent) {
            consider(t + opts.kill_grace);
        }
        if let Some(t) = drain_until {
            consider(t);
        }
        match rx.recv_timeout(wait) {
            Ok(Event::Out(b)) => output(tail, &b),
            Ok(Event::Eof) => eofs += 1,
            Ok(Event::Exit(s)) => {
                exit = Some(s);
                drain_until = Some(Instant::now() + DRAIN);
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => disconnected = true,
        }

        let now = Instant::now();
        if exit.is_none() {
            if killed_by.is_none() {
                if let Some(name) = forwarded.received() {
                    // launchd sends SIGTERM on bootout (hold, sync): stop the job and still record the run.
                    *killed_by = Some(name);
                    killpg(pgid, libc::SIGTERM);
                    term_at.get_or_insert(now);
                }
            }
            if !timed_out && now >= deadline {
                timed_out = true;
                killpg(pgid, libc::SIGTERM);
                term_at.get_or_insert(now);
            }
            if let Some(t) = term_at {
                if !kill_sent && now >= t + opts.kill_grace {
                    kill_sent = true;
                    killpg(pgid, libc::SIGKILL);
                }
            }
        }
        if disconnected || (exit.is_some() && (eofs >= 2 || drain_until.is_some_and(|d| now >= d))) {
            break;
        }
    }
    if timed_out || killed_by.is_some() {
        killpg(pgid, libc::SIGKILL); // make sure no stragglers survive in the group
    }
    Outcome { code: if timed_out { None } else { Some(exit.unwrap_or(1)) }, timed_out }
}

pub fn run_job(job: &Job, paths: &Paths, history: &History, opts: &RunOpts) -> i32 {
    let trigger = if opts.manual { Trigger::Manual } else { Trigger::Scheduled };
    let hist_err = |e: rusqlite::Error| {
        let _ = append_log(paths, &job.id, format!("hq: history error: {e}\n").as_bytes());
    };
    let skip = |reason: &str| {
        if let Err(e) = history.skip(&job.id, trigger, reason, (opts.now)().timestamp_millis()) {
            hist_err(e);
        }
        0
    };

    if !opts.manual && read_paused(paths).contains(&job.id) {
        return skip("paused");
    }
    if !opts.manual && in_quiet(job.quiet.as_ref(), (opts.now)()) {
        return skip("quiet");
    }
    let locked = match acquire_lock(paths, &job.id) {
        Lock::Held => true,
        Lock::Busy => return skip("overlap"),
        Lock::Unavailable(e) => {
            let _ = append_log(paths, &job.id, format!("hq: lock error, running without lock: {e}\n").as_bytes());
            false
        }
    };

    let started = (opts.now)();
    let started_ms = started.timestamp_millis();
    // Only the lock holder may call earlier "running" rows dead; unlocked, a live run could be wrongly failed.
    if locked {
        if let Err(e) = history.fail_stale(&job.id, started_ms, "hq: runner died before the run finished") {
            hist_err(e);
        }
    }
    let run_id = match history.start(&job.id, trigger, started_ms) {
        Ok(id) => Some(id),
        Err(e) => {
            hist_err(e);
            None
        }
    };
    let num = format!("{:04}", run_id.unwrap_or(0));
    rotate_if_needed(paths, &job.id, MAX_LOG_BYTES); // before the header, so the current log always holds the latest run
    let _ = append_log(
        paths,
        &job.id,
        format!("── RUN {num} · {} · {} ──\n", started.format("%Y-%m-%d %H:%M:%S"), trigger.as_str()).as_bytes(),
    );

    let mut tail = TailBuffer::new(20);
    let mut killed_by = None;
    let (code, status) = if !std::fs::metadata(&job.cwd).map(|m| m.is_dir()).unwrap_or(false) {
        let msg = format!("hq: working directory does not exist or is not a directory: {}\n", job.cwd.display());
        let _ = append_log(paths, &job.id, msg.as_bytes());
        tail.push(msg.as_bytes());
        (Some(1), Status::Failed)
    } else {
        let r = execute(job, paths, &mut tail, opts, &mut killed_by);
        if let Some(sig) = killed_by {
            let msg = format!("hq: killed by {sig}\n");
            let _ = append_log(paths, &job.id, msg.as_bytes());
            tail.push(msg.as_bytes());
        }
        let status = if r.timed_out {
            Status::Timeout
        } else if r.code == Some(0) && killed_by.is_none() {
            Status::Ok
        } else {
            Status::Failed
        };
        (r.code, status)
    };

    let ended = (opts.now)();
    let label = match status {
        Status::Ok => "NOMINAL",
        Status::Timeout => "TIMEOUT",
        _ => "FAULT",
    };
    let exit_text = code.map_or("killed".to_string(), |c| format!("exit {c}"));
    let elapsed = ended.timestamp_millis() - started_ms;
    let _ = append_log(
        paths,
        &job.id,
        format!("── END {num} · {label} · {} · {exit_text} ──\n", format_duration(elapsed)).as_bytes(),
    );
    if let Some(id) = run_id {
        if let Err(e) = history.finish(id, ended.timestamp_millis(), code, status, &tail.text()) {
            hist_err(e);
        }
    }
    if let Err(e) = history.prune(ended.timestamp_millis(), PRUNE_AGE_MS) {
        hist_err(e);
    }
    if locked {
        release_lock(paths, &job.id);
    }
    code.unwrap_or(TIMEOUT_EXIT)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{QuietWindow, Schedule};
    use crate::history::RunRow;
    use crate::state::set_paused;
    use chrono::TimeZone;
    use std::path::PathBuf;
    use std::sync::RwLock;

    /// A forwarded signal is process-wide: the signal test takes the write lock, every other run takes the read lock.
    static SIGNAL_GATE: RwLock<()> = RwLock::new(());

    struct Fx {
        dir: tempfile::TempDir,
        paths: Paths,
        history: History,
    }

    fn fx() -> Fx {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::temp(dir.path());
        let history = History::open(&paths.db()).unwrap();
        Fx { dir, paths, history }
    }

    fn job(f: &Fx, command: &str) -> Job {
        Job {
            id: "t".into(),
            description: String::new(),
            command: command.into(),
            cwd: f.dir.path().to_path_buf(),
            schedule: Schedule::Every { seconds: 300 },
            quiet: None,
            timeout_sec: 60,
            url: None,
        }
    }

    fn opts(manual: bool) -> RunOpts {
        RunOpts { manual, kill_grace: Duration::from_millis(200), now: Local::now }
    }

    fn run(f: &Fx, j: &Job, o: &RunOpts) -> i32 {
        let _g = SIGNAL_GATE.read().unwrap_or_else(|e| e.into_inner());
        run_job(j, &f.paths, &f.history, o)
    }

    fn last(f: &Fx) -> RunRow {
        f.history.recent("t", 1).unwrap().remove(0)
    }

    fn log(f: &Fx) -> String {
        String::from_utf8_lossy(&std::fs::read(f.paths.log("t")).unwrap()).into_owned()
    }

    fn write_lock(f: &Fx, content: &str) {
        std::fs::create_dir_all(f.paths.lock("t").parent().unwrap()).unwrap();
        std::fs::write(f.paths.lock("t"), content).unwrap();
    }

    #[test]
    fn ok_run() {
        let f = fx();
        assert_eq!(run(&f, &job(&f, "echo hi"), &opts(false)), 0);
        let r = last(&f);
        assert_eq!((r.status, r.exit_code, r.tail.as_deref(), r.trigger), (Status::Ok, Some(0), Some("hi"), Trigger::Scheduled));
        let l = log(&f);
        let id = format!("{:04}", r.id);
        let header = l.lines().next().unwrap();
        let pat = format!("── RUN {id} · ");
        assert!(header.starts_with(&pat) && header.ends_with(" · scheduled ──"), "{header}");
        let stamp = &header[pat.len()..header.len() - " · scheduled ──".len()];
        assert!(chrono::NaiveDateTime::parse_from_str(stamp, "%Y-%m-%d %H:%M:%S").is_ok(), "{stamp}");
        assert!(l.contains(&format!("── END {id} · NOMINAL · ")));
        assert!(l.contains("· exit 0 ──"));
    }

    #[test]
    fn failed_run() {
        let f = fx();
        assert_eq!(run(&f, &job(&f, "exit 3"), &opts(true)), 3);
        let r = last(&f);
        assert_eq!((r.status, r.exit_code, r.trigger), (Status::Failed, Some(3), Trigger::Manual));
        let l = log(&f);
        assert!(l.contains(" · FAULT · ") && l.contains("· exit 3 ──"), "{l}");
    }

    #[test]
    fn timeout_kills_the_job() {
        let f = fx();
        let mut j = job(&f, "sleep 30");
        j.timeout_sec = 1;
        let t0 = Instant::now();
        assert_eq!(run(&f, &j, &opts(false)), 124);
        assert_eq!(last(&f).status, Status::Timeout);
        assert!(t0.elapsed() < Duration::from_secs(3));
        let l = log(&f);
        assert!(l.contains("· TIMEOUT ·") && l.contains("· killed ──"), "{l}");
    }

    #[test]
    fn timeout_escalates_to_sigkill_when_term_is_ignored() {
        let f = fx();
        let mut j = job(&f, "trap '' TERM; sleep 30; echo late");
        j.timeout_sec = 1;
        let t0 = Instant::now();
        run(&f, &j, &opts(false));
        assert_eq!(last(&f).status, Status::Timeout);
        assert!(t0.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn held_job_is_skipped_but_manual_runs_anyway() {
        let f = fx();
        let marker = f.dir.path().join("ran");
        let j = job(&f, &format!("touch '{}'", marker.display()));
        set_paused(&f.paths, "t", true).unwrap();
        assert_eq!(run(&f, &j, &opts(false)), 0);
        let r = last(&f);
        assert_eq!((r.status, r.reason.as_deref()), (Status::Skipped, Some("paused")));
        assert!(!marker.exists());
        run(&f, &j, &opts(true));
        assert!(marker.exists());
    }

    fn two_am() -> DateTime<Local> {
        Local.with_ymd_and_hms(2026, 10, 17, 2, 0, 0).unwrap()
    }

    #[test]
    fn quiet_hours_skip() {
        let f = fx();
        let mut j = job(&f, "true");
        j.quiet = Some(QuietWindow { start: 23 * 60, end: 7 * 60 });
        run(&f, &j, &RunOpts { manual: false, kill_grace: Duration::from_millis(200), now: two_am });
        let r = last(&f);
        assert_eq!((r.status, r.reason.as_deref()), (Status::Skipped, Some("quiet")));
    }

    #[test]
    fn overlap_skip_when_a_live_process_holds_the_lock() {
        let f = fx();
        write_lock(&f, &std::process::id().to_string());
        run(&f, &job(&f, "true"), &opts(false));
        let r = last(&f);
        assert_eq!((r.status, r.reason.as_deref()), (Status::Skipped, Some("overlap")));
        assert!(f.paths.lock("t").exists(), "must not release a lock it does not hold");
    }

    #[test]
    fn stale_lock_is_taken_over() {
        let f = fx();
        write_lock(&f, "999999");
        run(&f, &job(&f, "true"), &opts(false));
        assert_eq!(last(&f).status, Status::Ok);
        assert!(!f.paths.lock("t").exists());
    }

    #[test]
    fn garbage_lock_is_taken_over() {
        let f = fx();
        write_lock(&f, "not a pid");
        run(&f, &job(&f, "true"), &opts(false));
        assert_eq!(last(&f).status, Status::Ok);
    }

    #[test]
    fn missing_cwd_is_a_recorded_failure() {
        let f = fx();
        let mut j = job(&f, "true");
        j.cwd = PathBuf::from("/nonexistent/dir");
        assert_eq!(run(&f, &j, &opts(false)), 1);
        let r = last(&f);
        assert_eq!(r.status, Status::Failed);
        assert!(r.tail.unwrap().contains("/nonexistent/dir"));
    }

    #[test]
    fn missing_command_is_a_recorded_failure() {
        let f = fx();
        assert_eq!(run(&f, &job(&f, "definitely-not-a-cmd"), &opts(false)), 127);
        assert_eq!(last(&f).status, Status::Failed);
    }

    #[test]
    fn tail_keeps_the_last_20_lines_including_a_partial_last_line() {
        let f = fx();
        run(&f, &job(&f, "seq 1 50; printf tail-no-newline"), &opts(false));
        let tail = last(&f).tail.unwrap();
        let lines: Vec<&str> = tail.split('\n').collect();
        assert_eq!(lines.len(), 20);
        assert_eq!(lines[19], "tail-no-newline");
        assert_eq!(lines[0], "32");
        let l = log(&f);
        assert!(l.contains("1\n2\n3\n") && l.contains("50\ntail-no-newline"));
    }

    #[test]
    fn user_tool_dirs_are_on_path() {
        let f = fx();
        run(&f, &job(&f, "echo $PATH"), &opts(false));
        let tail = last(&f).tail.unwrap();
        let home = std::env::var("HOME").unwrap();
        assert!(tail.contains(&format!("{home}/.local/bin")), "{tail}");
        assert!(tail.contains(&format!("{home}/.bun/bin")), "{tail}");
    }

    #[test]
    fn job_env_prefixes_path_and_survives_non_utf8() {
        use std::os::unix::ffi::OsStringExt;
        let os = |s: &str| OsString::from(s);
        let bad = OsString::from_vec(vec![b'x', 0xff, 0xfe]);
        let vars = vec![(os("A"), bad.clone()), (os("PATH"), os("/usr/bin:/bin")), (os("B"), os("b"))];
        let env = job_env_from(vars.into_iter(), Path::new("/h"));
        assert!(env.contains(&(os("A"), bad)));
        assert!(env.contains(&(os("B"), os("b"))));
        let paths: Vec<_> = env.iter().filter(|(k, _)| k == "PATH").collect();
        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0].1, os("/h/.local/bin:/h/.bun/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin"));
        let unset = job_env_from(std::iter::empty(), Path::new("/h"));
        assert_eq!(unset, vec![(os("PATH"), os("/h/.local/bin:/h/.bun/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin"))]);
        assert_eq!(job_env().iter().filter(|(k, _)| k == "PATH").count(), 1);
    }

    #[test]
    fn cwd_that_is_a_file_is_a_recorded_failure() {
        let f = fx();
        let file = f.dir.path().join("not-a-dir");
        std::fs::write(&file, "").unwrap();
        let mut j = job(&f, "echo hi");
        j.cwd = file;
        assert_eq!(run(&f, &j, &opts(false)), 1);
        let r = last(&f);
        assert_eq!(r.status, Status::Failed);
        assert!(r.tail.unwrap().contains("not a directory"));
        assert!(log(&f).contains("── END "));
        assert!(!f.paths.lock("t").exists());
    }

    #[test]
    fn stale_running_row_is_closed_by_the_next_run() {
        let f = fx();
        let stale = f.history.start("t", Trigger::Scheduled, Local::now().timestamp_millis() - 60_000).unwrap();
        run(&f, &job(&f, "true"), &opts(false));
        let row = f.history.recent("t", 10).unwrap().into_iter().find(|r| r.id == stale).unwrap();
        assert_eq!(row.status, Status::Failed);
        assert!(row.tail.unwrap().contains("runner died"));
        assert!(row.ended_at.is_some());
    }

    #[test]
    fn unlocked_run_leaves_running_rows_alone() {
        let f = fx();
        // a file where the locks directory should be makes the lock unavailable
        std::fs::create_dir_all(&f.paths.home).unwrap();
        std::fs::write(f.paths.home.join("locks"), "x").unwrap();
        let live = f.history.start("t", Trigger::Scheduled, Local::now().timestamp_millis() - 60_000).unwrap();
        assert_eq!(run(&f, &job(&f, "true"), &opts(false)), 0);
        assert!(log(&f).contains("running without lock"));
        let row = f.history.recent("t", 10).unwrap().into_iter().find(|r| r.id == live).unwrap();
        assert_eq!(row.status, Status::Running);
    }

    #[test]
    fn absurd_timeout_does_not_panic() {
        let f = fx();
        let mut j = job(&f, "true");
        j.timeout_sec = u64::MAX;
        assert_eq!(run(&f, &j, &opts(false)), 0);
        assert_eq!(last(&f).status, Status::Ok);
    }

    #[test]
    fn escaped_grandchild_holding_the_pipe_does_not_hang_the_run() {
        let f = fx();
        let t0 = Instant::now();
        run(&f, &job(&f, "perl -MPOSIX -e 'fork and exit; POSIX::setsid(); sleep 20'; echo done"), &opts(false));
        assert!(t0.elapsed() < Duration::from_secs(6));
        let r = last(&f);
        assert_eq!((r.status, r.tail.as_deref()), (Status::Ok, Some("done")));
    }

    #[test]
    fn log_rotates_past_1_mb() {
        let f = fx();
        std::fs::create_dir_all(f.paths.log("t").parent().unwrap()).unwrap();
        std::fs::write(f.paths.log("t"), "x".repeat(1_150_000)).unwrap();
        run(&f, &job(&f, "echo hi"), &opts(false));
        assert!(f.paths.log("t").with_extension("log.1").exists());
        assert!(std::fs::metadata(f.paths.log("t")).unwrap().len() < 1_048_576);
    }

    #[test]
    fn non_utf8_and_split_multibyte_output() {
        let f = fx();
        assert_eq!(run(&f, &job(&f, r"printf '\xff\xe2\x94'; sleep 0.1; printf '\x80 done'"), &opts(false)), 0);
        let raw = std::fs::read(f.paths.log("t")).unwrap();
        let want = b"\xff\xe2\x94\x80 done";
        assert!(raw.windows(want.len()).any(|w| w == want), "raw bytes must be preserved");
        let tail = last(&f).tail.unwrap();
        assert!(tail.starts_with('\u{FFFD}') && tail.ends_with(" done") && tail.contains('─'), "{tail:?}");
    }

    #[test]
    fn forwarded_sigterm_stops_the_job_and_is_recorded_as_failed() {
        let f = fx();
        let marker = f.dir.path().join("started");
        let j = job(&f, &format!("sh -c 'trap \"\" TERM; echo $$ > \"{}\"; sleep 30' & wait", marker.display()));
        let _g = SIGNAL_GATE.write().unwrap_or_else(|e| e.into_inner());
        let m = marker.clone();
        let sender = std::thread::spawn(move || {
            for _ in 0..200 {
                if std::fs::read_to_string(&m).is_ok_and(|s| s.trim().parse::<i32>().is_ok()) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            // SAFETY: the handlers are registered before the job (and so the marker) exists.
            unsafe { libc::kill(libc::getpid(), libc::SIGTERM) };
        });
        let t0 = Instant::now();
        let code = run_job(&j, &f.paths, &f.history, &opts(false));
        sender.join().unwrap();
        assert!(t0.elapsed() < Duration::from_secs(5));
        assert_ne!(code, 0);
        assert_eq!(last(&f).status, Status::Failed);
        let l = log(&f);
        assert!(l.contains("hq: killed by SIGTERM") && l.contains(" · FAULT · "), "{l}");
        assert!(!f.paths.lock("t").exists());
        // The TERM-ignoring grandchild must have been SIGKILLed with the group, not orphaned.
        let pid: i32 = std::fs::read_to_string(&marker).unwrap().trim().parse().unwrap();
        let mut gone = false;
        for _ in 0..40 {
            if !is_alive(pid) {
                gone = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(gone, "grandchild {pid} survived");
    }

    #[test]
    fn history_error_is_logged_and_the_job_still_runs() {
        let f = fx();
        rusqlite::Connection::open(f.paths.db()).unwrap().execute_batch("DROP TABLE runs").unwrap();
        assert_eq!(run(&f, &job(&f, "echo hi"), &opts(false)), 0);
        let l = log(&f);
        assert!(l.contains("hq: history error: "), "{l}");
        assert!(l.contains("── RUN 0000 · ") && l.contains("── END 0000 · NOMINAL · "), "{l}");
        assert!(l.contains("hi\n"));
    }

    #[test]
    fn format_duration_values() {
        assert_eq!(format_duration(0), "0s");
        assert_eq!(format_duration(41_000), "41s");
        assert_eq!(format_duration(59_499), "59s");
        assert_eq!(format_duration(66_000), "1m06s");
        assert_eq!(format_duration(-5), "0s");
    }
}
