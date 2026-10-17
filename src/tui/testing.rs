//! Shared test helpers for the views: the TS `snapshot.ts` fixture, a recording `Effects`, buffer-to-text.

use super::Effects;
use crate::config::{Job, QuietWindow, Schedule};
use crate::history::{RunRow, Status, Trigger};
use crate::launchd::AgentState;
use crate::snapshot::{JobView, Snapshot};
use crate::stacks::Docker;
use chrono::{DateTime, Duration, Local, TimeZone};
use ratatui::buffer::Buffer;
use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::Arc;
use unicode_width::UnicodeWidthStr;

#[derive(Default)]
pub struct Rec {
    pub cli: RefCell<Vec<Vec<String>>>,
    pub opened: RefCell<Vec<String>>,
    /// The docker the keys get; by default one that refuses, so a test can never reach the real CLI.
    pub docker: Option<Arc<dyn Docker>>,
}

struct NoDocker;

impl Docker for NoDocker {
    fn exec(&self, _: &[String], _: Option<std::time::Duration>) -> Result<crate::launchd::CmdOutput, String> {
        Err("docker disabled in tests".into())
    }
}

impl Effects for Rec {
    fn spawn_cli(&self, args: &[&str]) {
        self.cli.borrow_mut().push(args.iter().map(|s| s.to_string()).collect());
    }
    fn open(&self, url: &str) {
        self.opened.borrow_mut().push(url.to_string());
    }
    fn docker(&self) -> Arc<dyn Docker> {
        self.docker.clone().unwrap_or_else(|| Arc::new(NoDocker))
    }
}

/// Each buffer row as text; a wide glyph counts once (its covered cell is skipped).
pub fn lines(buf: &Buffer) -> Vec<String> {
    let w = buf.area.width as usize;
    (0..buf.area.height as usize)
        .map(|y| {
            let mut s = String::new();
            let mut skip = 0;
            for x in 0..w {
                if skip > 0 {
                    skip -= 1;
                    continue;
                }
                let sym = buf.content[y * w + x].symbol();
                skip = sym.width().saturating_sub(1);
                s.push_str(sym);
            }
            s
        })
        .collect()
}

/// (column, row) of the first occurrence of `text`, in display columns.
pub fn find(lines: &[String], text: &str) -> (u16, u16) {
    for (y, l) in lines.iter().enumerate() {
        if let Some(i) = l.find(text) {
            return (l[..i].width() as u16, y as u16);
        }
    }
    panic!("{text:?} not on screen:\n{}", lines.join("\n"));
}

pub fn now() -> DateTime<Local> {
    Local.with_ymd_and_hms(2026, 10, 16, 17, 32, 8).unwrap()
}

fn t(h: u32, m: u32) -> i64 {
    Local.with_ymd_and_hms(2026, 10, 16, h, m, 0).unwrap().timestamp_millis()
}

fn run(job: &str, id: i64, status: Status, started: i64, secs: i64, exit: Option<i32>, tail: &str) -> RunRow {
    RunRow {
        id,
        job: job.into(),
        trigger: Trigger::Scheduled,
        started_at: started,
        ended_at: Some(started + secs * 1000),
        exit_code: exit,
        status,
        reason: (status == Status::Skipped).then(|| "quiet".to_string()),
        tail: Some(tail.into()),
    }
}

pub fn job(id: &str, schedule: Schedule) -> Job {
    Job {
        id: id.into(),
        description: String::new(),
        command: "true".into(),
        cwd: PathBuf::from("/tmp"),
        schedule,
        quiet: None,
        timeout_sec: 600,
        url: None,
    }
}

const LINKED: AgentState = AgentState { linked: true, running: false, last_exit: Some(0) };
fn unlinked() -> AgentState {
    AgentState { linked: false, running: false, last_exit: None }
}

pub fn views() -> Vec<JobView> {
    let n = now();
    let eink = vec![
        run("eink-weather", 1, Status::Ok, t(17, 30), 41, Some(0), "connecting\npushed (battery 2921 mV, clock drift -3s), fallback armed for midnight"),
        run("eink-weather", 2, Status::Failed, t(17, 0), 50, Some(1), ""),
        run("eink-weather", 3, Status::Skipped, t(16, 30), 0, None, ""),
    ];
    let ts = vec![run("tailscale-watchdog", 4, Status::Failed, t(17, 5), 9, Some(1), "tailscale: not running")];
    let mut wx = job("eink-weather", Schedule::Every { seconds: 1800 });
    wx.description = "Punggol weather → nRF e-ink".into();
    wx.command = "uv run python -m epd.cli weather --push".into();
    wx.cwd = PathBuf::from("/Users/x/Developer/potateros/nrf5-eink");
    wx.quiet = Some(QuietWindow { start: 1380, end: 420 });
    wx.timeout_sec = 300;
    vec![
        JobView { job: wx, held: false, agent: LINKED, synced: true, last: Some(eink[0].clone()), recent: eink, running: false, next: Some(n + Duration::milliseconds(1_672_000)) },
        JobView {
            job: job("tailscale-watchdog", Schedule::Every { seconds: 3600 }),
            held: false,
            agent: AgentState { last_exit: Some(1), ..LINKED },
            synced: true,
            last: Some(ts[0].clone()),
            recent: ts,
            running: false,
            next: Some(n + Duration::milliseconds(1_972_000)),
        },
        JobView { job: job("eink-wipe", Schedule::At { times: vec![(6, 55)] }), held: true, agent: unlinked(), synced: true, last: None, recent: vec![], running: false, next: None },
        JobView {
            job: job("trml-reminders", Schedule::Every { seconds: 300 }),
            held: false,
            agent: unlinked(),
            synced: false,
            last: Some(run("trml-reminders", 5, Status::Ok, t(17, 30), 2, Some(0), "")),
            recent: vec![],
            running: false,
            next: Some(n + Duration::milliseconds(172_000)),
        },
    ]
}

pub fn snapshot() -> Snapshot {
    Snapshot { jobs: views(), config_error: None, history_error: None, at: now() }
}
