#![allow(dead_code)]

mod config;
mod history;
mod launchd;
mod log;
mod paths;
mod runner;
mod schedule;
mod snapshot;
mod stacks;
mod state;
mod tui;

use chrono::{Local, TimeZone};
use config::{load_config, Job};
use history::{History, Status, Trigger};
use launchd::{apply_sync, hold, label, launchctl_from_env, plan_sync, resume, uninstall, PlistCtx};
use log::append_log;
use paths::Paths;
use runner::{run_job, RunOpts};
use snapshot::{fetch_agents, load_snapshot, JobView};
use std::io::{Read, Seek, SeekFrom, Write};
use std::time::Duration;

const USAGE: &str = "usage: hq [status [--json] | run <job> [--manual] | logs <job> [-f] | hold <job> | resume <job> | sync [--yes] | uninstall]";

fn fail(msg: impl std::fmt::Display, code: i32) -> i32 {
    eprintln!("hq: {msg}");
    code
}

fn hhmm(ms: Option<i64>) -> String {
    match ms.and_then(|t| Local.timestamp_millis_opt(t).single()) {
        Some(d) => d.format("%H:%M").to_string(),
        None => "—".to_string(),
    }
}

fn status_word(v: &JobView) -> &'static str {
    if v.held {
        "HOLD"
    } else if let Some(last) = &v.last {
        if matches!(last.status, Status::Ok | Status::Running) {
            "OK"
        } else {
            "FAULT"
        }
    } else {
        "NEVER"
    }
}

fn pad(s: &str, w: usize) -> String {
    format!("{s}{}", " ".repeat(w.saturating_sub(s.chars().count())))
}

/// Looks up a job; Err is the exit code after the message has been printed.
fn find_job(paths: &Paths, id: Option<&str>) -> Result<Job, i32> {
    let jobs = load_config(&paths.config).map_err(|e| fail(e, 1))?;
    jobs.into_iter().find(|j| Some(j.id.as_str()) == id).ok_or_else(|| fail(format!("unknown job {}", id.unwrap_or("(none)")), 2))
}

/// The history plus the reason the real database could not be opened. When even the in-memory
/// fallback fails there is no history at all and the error is the whole story.
fn open_history(paths: &Paths) -> (Option<History>, Option<String>) {
    match History::open(&paths.db()) {
        Ok(h) => (Some(h), None),
        Err(e) => match History::in_memory() {
            Ok(h) => (Some(h), Some(e.to_string())),
            Err(e2) => (None, Some(format!("{e}; {e2}"))),
        },
    }
}

/// Writes one line to stdout. Err carries the exit code: a closed pipe (`hq status | head -1`) is a quiet 0.
fn emit(line: &str) -> Result<(), i32> {
    match writeln!(std::io::stdout().lock(), "{line}") {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => Err(0),
        Err(e) => Err(fail(e, 1)),
    }
}

fn status(paths: &Paths, json: bool) -> i32 {
    let (history, open_err) = open_history(paths);
    let Some(history) = history else {
        return fail(format!("history unavailable: {}", open_err.unwrap_or_default()), 1);
    };
    let lc = launchctl_from_env(paths);
    let jobs = load_config(&paths.config).unwrap_or_default();
    let agents = fetch_agents(&jobs, lc.as_ref());
    let snap = load_snapshot(paths, &history, &agents, Local::now(), None);
    if let Some(e) = snap.config_error {
        return fail(e, 1);
    }
    let printed = (|| {
        if json {
            let v = serde_json::Value::Array(snap.jobs.iter().map(JobView::to_json).collect());
            emit(&serde_json::to_string_pretty(&v).unwrap_or_default())
        } else {
            let w = snap.jobs.iter().map(|v| v.job.id.chars().count()).max().unwrap_or(0).max(4);
            for v in &snap.jobs {
                emit(&format!(
                    "{}  {}  last {}  next {}",
                    pad(&v.job.id, w),
                    pad(status_word(v), 5),
                    hhmm(v.last.as_ref().map(|r| r.started_at)),
                    hhmm(v.next.map(|d| d.timestamp_millis()))
                ))?;
            }
            Ok(())
        }
    })();
    if let Err(c) = printed {
        return c;
    }
    match open_err.or(snap.history_error) {
        Some(e) => fail(format!("history unavailable: {e}"), 1),
        None => 0,
    }
}

fn run(paths: &Paths, arg: Option<&str>, manual: bool) -> i32 {
    let (history, history_err) = open_history(paths);
    let job = match load_config(&paths.config) {
        Ok(jobs) => match jobs.into_iter().find(|j| Some(j.id.as_str()) == arg) {
            Some(j) => j,
            None => return fail(format!("unknown job {}", arg.unwrap_or("(none)")), 2),
        },
        Err(e) => {
            // Under launchd nobody sees stderr: leave the error where hq status and the log will show it.
            if let Some(id) = arg {
                let at = Local::now().timestamp_millis();
                let _ = append_log(paths, id, format!("hq: config error: {e}\n").as_bytes());
                let trigger = if manual { Trigger::Manual } else { Trigger::Scheduled };
                if let Some(Ok(rid)) = history.as_ref().map(|h| h.start(id, trigger, at)) {
                    let _ = history.as_ref().map(|h| h.finish(rid, at, Some(1), Status::Failed, &format!("config error: {e}")));
                }
            }
            return fail(e, 1);
        }
    };
    if let Some(e) = &history_err {
        let _ = append_log(paths, &job.id, format!("hq: history unavailable: {e}\n").as_bytes());
    }
    let Some(history) = history else {
        return fail(format!("history unavailable: {}", history_err.unwrap_or_default()), 1);
    };
    let opts = RunOpts { manual, kill_grace: Duration::from_secs(10), now: Local::now };
    run_job(&job, paths, &history, &opts)
}

fn read_from(path: &std::path::Path, offset: u64) -> Vec<u8> {
    let mut buf = Vec::new();
    if let Ok(mut f) = std::fs::File::open(path) {
        if f.seek(SeekFrom::Start(offset)).is_ok() {
            let _ = f.read_to_end(&mut buf);
        }
    }
    buf
}

fn logs(paths: &Paths, arg: Option<&str>, follow: bool) -> i32 {
    let job = match find_job(paths, arg) {
        Ok(j) => j,
        Err(c) => return c,
    };
    let path = paths.log(&job.id);
    let size = |p: &std::path::Path| std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
    let mut out = std::io::stdout();
    let first = read_from(&path, 0);
    if let Err(e) = out.write_all(&first).and_then(|_| out.flush()) {
        return if e.kind() == std::io::ErrorKind::BrokenPipe { 0 } else { fail(e, 1) };
    }
    if !follow {
        return 0;
    }
    let mut shown = first.len() as u64;
    loop {
        std::thread::sleep(Duration::from_millis(500));
        let now = size(&path);
        if now == shown {
            continue;
        }
        let from = if now < shown { 0 } else { shown }; // shrank: rotated, reprint from the start
        let chunk = read_from(&path, from);
        if out.write_all(&chunk).and_then(|_| out.flush()).is_err() {
            return 0; // reader went away
        }
        shown = from + chunk.len() as u64;
    }
}

fn confirm(question: &str) -> bool {
    print!("{question} [y/N] ");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    matches!(std::io::stdin().read_line(&mut line), Ok(n) if n > 0) && line.trim().eq_ignore_ascii_case("y")
}

fn sync(paths: &Paths, yes: bool) -> i32 {
    let jobs = match load_config(&paths.config) {
        Ok(j) => j,
        Err(e) => return fail(e, 1),
    };
    let ctx = PlistCtx::current(paths);
    let plan = match plan_sync(&jobs, &ctx, paths) {
        Ok(p) => p,
        Err(e) => return fail(e, 1),
    };
    let said = (|| {
        if plan.add.is_empty() && plan.change.is_empty() && plan.remove.is_empty() {
            emit("in sync")?;
            return Ok(true);
        }
        for id in &plan.add {
            emit(&format!("+ {id}"))?;
        }
        for id in &plan.change {
            emit(&format!("~ {id}"))?;
        }
        for id in &plan.remove {
            emit(&format!("- {id}  ({})", label(id)))?;
        }
        Ok(false)
    })();
    match said {
        Err(c) => return c,
        Ok(true) => return 0,
        Ok(false) => {}
    }
    if !yes && !confirm("Apply?") {
        return 1;
    }
    match apply_sync(&plan, &jobs, &ctx, paths, launchctl_from_env(paths).as_ref()) {
        Ok(()) => emit("synced").err().unwrap_or(0),
        Err(e) => fail(e, 1),
    }
}

fn dispatch(args: &[String]) -> i32 {
    let rest = args.get(1..).unwrap_or(&[]);
    let flag = |f: &str| rest.iter().any(|a| a == f);
    let arg = rest.iter().find(|a| !a.starts_with('-')).map(String::as_str);
    let paths = Paths::from_env();
    match args.first().map(String::as_str) {
        None => {
            if !std::io::IsTerminal::is_terminal(&std::io::stdout()) {
                eprintln!("hq: the TUI needs a terminal");
                return 2;
            }
            match tui::run(paths) {
                Ok(()) => 0,
                Err(e) => fail(e, 1),
            }
        }
        Some("status") => status(&paths, flag("--json")),
        Some("run") => run(&paths, arg, flag("--manual")),
        Some("logs") => logs(&paths, arg, flag("-f")),
        Some("hold") => match find_job(&paths, arg) {
            Err(c) => c,
            Ok(job) => match hold(&job.id, &paths, launchctl_from_env(&paths).as_ref()) {
                Ok(()) => {
                    println!("{} on hold", job.id);
                    0
                }
                Err(e) => fail(e, 1),
            },
        },
        Some("resume") => match find_job(&paths, arg) {
            Err(c) => c,
            Ok(job) => match resume(&job, &PlistCtx::current(&paths), &paths, launchctl_from_env(&paths).as_ref()) {
                Ok(()) => {
                    println!("{} resumed", job.id);
                    0
                }
                Err(e) => fail(e, 1),
            },
        },
        Some("sync") => sync(&paths, flag("--yes")),
        Some("uninstall") => match uninstall(&paths, launchctl_from_env(&paths).as_ref()) {
            Ok(()) => {
                println!("removed hq agents; restored migrated/ plists");
                0
            }
            Err(e) => fail(e, 1),
        },
        Some(_) => {
            eprintln!("{USAGE}");
            2
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    std::process::exit(dispatch(&args));
}
