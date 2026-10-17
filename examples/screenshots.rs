//! Regenerate docs/images/*.png: draws the real views with demo data into a ratatui `TestBackend`,
//! converts the cells to HTML and screenshots that with headless Chrome. Port of tools/screenshots.tsx.
//!
//!   cargo run --example screenshots

use chrono::{DateTime, Local, TimeZone};
use hq::config::{Job, QuietWindow, Schedule};
use hq::history::{RunRow, Status, Trigger};
use hq::launchd::AgentState;
use hq::snapshot::{JobView, Snapshot};
use hq::stacks::{ServiceState, ServiceView, Stack, StackView};
use hq::tui::logview::LogState;
use hq::tui::{draw, App, Mode, PendingKind, View};
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::style::{Color, Modifier};
use ratatui::Terminal;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;
use unicode_width::UnicodeWidthStr;

const CHROME: &str = "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome";
const BG: &str = "#0b1216";
const FONT_PX: u32 = 14;
const CHAR_W: f64 = 8.43; // Menlo advance at 14 px
const LINE_H: u32 = 18;
const PAD: u32 = 24;
/// The terminal is `MAX_WIDTH` (90) wide, as the TS tool's was; the content ends at column 88, which sizes the PNG.
const TERM_COLS: u16 = 90;
const SHOT_COLS: u16 = 88;

// ─── demo data (tools/demo.ts) ───────────────────────────────────────────────

fn at(h: u32, m: u32, s: u32) -> DateTime<Local> {
    Local.with_ymd_and_hms(2026, 10, 16, h, m, s).unwrap()
}

struct Demo {
    next_id: i64,
}

impl Demo {
    fn run(&mut self, job: &str, status: Status, started: DateTime<Local>, secs: i64, exit: Option<i32>, tail: &str) -> RunRow {
        self.next_id += 1;
        let started_at = started.timestamp_millis();
        RunRow {
            id: self.next_id,
            job: job.into(),
            trigger: Trigger::Scheduled,
            started_at,
            ended_at: (status != Status::Running).then(|| started_at + secs * 1000),
            exit_code: exit,
            status,
            reason: (status == Status::Skipped).then(|| "quiet".to_string()),
            tail: Some(tail.into()),
        }
    }

    /// `n` runs, newest first, `mins` apart, starting at `from`.
    fn every(&mut self, from: DateTime<Local>, mins: i64, n: usize, mut f: impl FnMut(&mut Demo, DateTime<Local>, usize) -> RunRow) -> Vec<RunRow> {
        (0..n).map(|i| f(self, from - chrono::Duration::minutes(mins * i as i64), i)).collect()
    }
}

fn job(id: &str, description: &str, cwd: &str, command: &str, schedule: Schedule, timeout_sec: u64) -> Job {
    Job {
        id: id.into(),
        description: description.into(),
        command: command.into(),
        cwd: PathBuf::from(cwd),
        schedule,
        quiet: None,
        timeout_sec,
        url: None,
    }
}

fn view(job: Job, recent: Vec<RunRow>, next: DateTime<Local>, agent: AgentState) -> JobView {
    JobView {
        job,
        held: false,
        agent,
        synced: true,
        running: recent.first().is_some_and(|r| r.status == Status::Running),
        last: recent.iter().find(|r| r.status != Status::Skipped).cloned(),
        recent,
        next: Some(next),
    }
}

fn snapshot(now: DateTime<Local>) -> Snapshot {
    let mut d = Demo { next_id: 0 };
    let linked = || AgentState { linked: true, running: false, last_exit: Some(0) };
    let weather = d.every(at(21, 30, 0), 30, 10, |d, t, i| {
        if i == 6 {
            d.run("eink-weather", Status::Failed, t, 52, Some(1), "BleakDeviceNotFoundError: NRF_EPD_5E5D not found after 45 s")
        } else {
            d.run("eink-weather", Status::Ok, t, 74 + (i as i64 % 3), Some(0), "pushed proverb (battery 2917 mV, clock drift -2s), fallback armed for midnight")
        }
    });
    let wipe = vec![d.run("eink-wipe", Status::Ok, at(6, 55, 0), 98, Some(0), "pushed proverb after CLEAR (battery 2931 mV), fallback armed for midnight")];
    let mut tailscale = vec![d.run("tailscale-watchdog", Status::Failed, at(21, 16, 0), 31, Some(1), "2026-10-16 21:16:53  ERROR 'tailscale up' timed out after 30s")];
    tailscale.extend(d.every(at(20, 16, 0), 60, 6, |d, t, _| d.run("tailscale-watchdog", Status::Ok, t, 1, Some(0), "")));
    let mut reminders = vec![d.run("trml-reminders", Status::Running, at(21, 32, 2), 0, None, "")];
    reminders.extend(d.every(at(21, 27, 0), 5, 9, |d, t, _| d.run("trml-reminders", Status::Ok, t, 5, Some(0), "117 reminders -> host/data/reminders.json")));

    let mut wx = job("eink-weather", "Punggol weather → nRF e-ink", "~/Developer/potateros/nrf5-eink", "uv run python -m epd.cli --push", Schedule::Every { seconds: 1800 }, 300);
    wx.quiet = Some(QuietWindow { start: 1380, end: 420 });
    let wipe_job = job("eink-wipe", "Daily full clear of the e-ink (ghosting)", "~/Developer/potateros/nrf5-eink", "uv run python -m epd.cli --push --wipe", Schedule::At { times: vec![(6, 55)] }, 300);
    let ts_job = job("tailscale-watchdog", "Reconnect Tailscale if it drops", "~/Developer/potateros/trml-byos", "host/tailscale-watchdog.sh", Schedule::Every { seconds: 3600 }, 120);
    let rem_job = job("trml-reminders", "Export macOS Reminders for the TRMNL screen", "~/Developer/potateros/trml-byos", "host/reminders-export.sh", Schedule::Every { seconds: 300 }, 120);
    let next_day = at(6, 55, 0) + chrono::Duration::days(1);
    Snapshot {
        at: now,
        config_error: None,
        history_error: None,
        jobs: vec![
            view(wx, weather, at(22, 0, 0), linked()),
            view(wipe_job, wipe, next_day, linked()),
            view(ts_job, tailscale, at(22, 16, 0), AgentState { last_exit: Some(1), ..linked() }),
            view(rem_job, reminders, at(21, 37, 0), linked()),
        ],
    }
}

const DEMO_LOG: &str = "── RUN 0412 · 2026-10-16 21:00:03 · scheduled ──
scanning for NRF_EPD_5E5D…
connected (fw v1.10-nrf52811, battery 2919 mV)
rendering proverb · uploading 2 planes (rle 3.1 KB)
refresh · holding the link 30 s
pushed proverb (battery 2919 mV, clock drift -2s), fallback armed for midnight
── END 0412 · NOMINAL · 1m15s · exit 0 ──
── RUN 0418 · 2026-10-16 21:30:02 · scheduled ──
scanning for NRF_EPD_5E5D…
connected (fw v1.10-nrf52811, battery 2917 mV)
rendering proverb · uploading 2 planes (rle 3.0 KB)
refresh · holding the link 30 s
pushed proverb (battery 2917 mV, clock drift -2s), fallback armed for midnight
── END 0418 · NOMINAL · 1m14s · exit 0 ──
";

const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
const MIB: f64 = 1024.0 * 1024.0;

#[allow(clippy::too_many_arguments)]
fn svc(stack: &str, service: &str, image: &str, ports: &[u16], state: ServiceState, cpu: Option<f64>, mem: Option<f64>, status: &str, exit_code: i32) -> ServiceView {
    ServiceView {
        stack: stack.into(),
        service: service.into(),
        container: (state != ServiceState::Missing).then(|| service.to_string()),
        image: image.into(),
        ports: ports.to_vec(),
        state,
        health: String::new(),
        status: status.into(),
        exit_code,
        cpu,
        mem: mem.map(|m| m as u64),
    }
}

fn stacks() -> Vec<StackView> {
    use ServiceState::*;
    let h = |s: &str, i: &str, p: &[u16], st, cpu, mem: Option<f64>, status: &str| svc("homelab", s, i, p, st, cpu, mem, status, 0);
    let homelab = vec![
        h("jellyfin", "jellyfin/jellyfin:latest", &[8096], Running, Some(84.2), Some(1.9 * GIB), "Up 6 hours"),
        h("radarr", "lscr.io/linuxserver/radarr:latest", &[7878], Running, Some(3.1), Some(412.0 * MIB), "Up 6 hours"),
        h("sonarr", "lscr.io/linuxserver/sonarr:latest", &[8989], Running, Some(2.4), Some(388.0 * MIB), "Up 6 hours"),
        h("prowlarr", "lscr.io/linuxserver/prowlarr:latest", &[9696], Exited, None, None, "Exited (0) 2 hours ago"),
        h("qbittorrent", "lscr.io/linuxserver/qbittorrent:latest", &[8080], Exited, None, None, "Exited (0) 2 hours ago"),
        h("tdarr", "haveagitgat/tdarr:latest", &[8265], Running, Some(61.7), Some(1.2 * GIB), "Up 6 hours"),
        svc("homelab", "immich-server", "ghcr.io/immich-app/immich-server:release", &[2283], Restarting, Some(0.0), Some(96.0 * MIB), "Restarting (1) 4 seconds ago", 1),
        h("vaultwarden", "vaultwarden/server:latest", &[8222], Running, Some(0.1), Some(34.0 * MIB), "Up 2 days"),
        h("korrosync", "szaffarano/korrosync:latest", &[7200], Running, Some(0.0), Some(0.9 * MIB), "Up 39 hours"),
        h("navidrome", "deluan/navidrome:latest", &[4533], Missing, None, None, "no container"),
    ];
    let t = |s: &str, p: &[u16], cpu, mem: f64, image: &str| svc("terminus", s, image, p, Running, Some(cpu), Some(mem), "Up 39 hours", 0);
    let terminus = vec![
        t("web", &[2300], 0.2, 311.0 * MIB, "ghcr.io/usetrmnl/terminus:latest"),
        t("worker", &[], 0.3, 87.0 * MIB, "ghcr.io/usetrmnl/terminus:latest"),
        t("database", &[5432], 0.0, 50.0 * MIB, "postgres:17"),
    ];
    let stack = |name: &str, dir: &str| Stack { name: name.into(), dir: PathBuf::from(dir), files: vec![], env_file: None };
    vec![
        StackView { stack: stack("homelab", "~/Developer/potateros/homelab"), services: homelab, error: None },
        StackView { stack: stack("terminus", "~/Developer/potateros/trml-byos"), services: terminus, error: None },
    ]
}

// ─── buffer → HTML ───────────────────────────────────────────────────────────

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// One `<span>` per run of cells with the same colour/weight; wide glyphs cover the cell after them.
fn buffer_to_html(buf: &Buffer, rows: u16) -> String {
    let w = buf.area.width as usize;
    let mut html = String::new();
    for y in 0..rows as usize {
        let mut runs: Vec<(String, String)> = Vec::new();
        let mut skip = 0;
        for x in 0..w {
            if skip > 0 {
                skip -= 1;
                continue;
            }
            let cell = &buf.content[y * w + x];
            let sym = cell.symbol();
            skip = sym.width().saturating_sub(1);
            let mut style = String::new();
            if let Color::Rgb(r, g, b) = cell.fg {
                let _ = write!(style, "color:rgb({r},{g},{b})");
            }
            if cell.modifier.contains(Modifier::BOLD) {
                style.push_str(";font-weight:bold");
            }
            if cell.modifier.contains(Modifier::DIM) {
                style.push_str(";opacity:.6");
            }
            match runs.last_mut() {
                Some((s, text)) if *s == style => text.push_str(sym),
                _ => runs.push((style, sym.to_string())),
            }
        }
        if let Some((s, t)) = runs.last_mut() {
            if s.is_empty() {
                *t = t.trim_end().to_string();
            }
        }
        for (style, text) in runs {
            if style.is_empty() {
                html.push_str(&esc(&text));
            } else {
                let _ = write!(html, "<span style=\"{}\">{}</span>", style.trim_start_matches(';'), esc(&text));
            }
        }
        html.push('\n');
    }
    html
}

fn shoot(name: &str, rows: u16, out: &Path, tmp: &Path, app: &mut App) {
    let mut term = Terminal::new(TestBackend::new(TERM_COLS, rows)).unwrap();
    term.draw(|f| draw(f, app)).unwrap();
    let buf = term.backend().buffer();
    // Crop to the content plus one blank row, like the TS tool, which printed only the frame's lines.
    let last = (0..rows).rev().find(|&y| (0..TERM_COLS).any(|x| buf[(x, y)].symbol() != " ")).unwrap_or(0);
    let rows = rows.min(last + 2);
    let html = buffer_to_html(buf, rows);
    let width = (SHOT_COLS as f64 * CHAR_W).ceil() as u32 + 2 * PAD;
    let height = rows as u32 * LINE_H + 2 * PAD;
    let page = tmp.join(format!("{name}.html"));
    std::fs::write(
        &page,
        format!(
            "<!doctype html><meta charset=\"utf-8\"><style>\n      html,body{{margin:0;background:{BG}}}\n      pre{{margin:0;padding:{PAD}px;font:{FONT_PX}px/{LINE_H}px Menlo,monospace;color:#7f9aa5}}\n    </style><pre>{html}</pre>"
        ),
    )
    .unwrap();
    let png = out.join(format!("{name}.png"));
    let status = Command::new(CHROME)
        .args(["--headless=new", "--disable-gpu", "--hide-scrollbars", "--force-device-scale-factor=2"])
        .arg(format!("--window-size={width},{height}"))
        .arg(format!("--screenshot={}", png.display()))
        .arg(format!("file://{}", page.display()))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("run chrome");
    assert!(status.success(), "chrome failed for {name}");
    println!("{}  ({SHOT_COLS}×{rows})", png.display());
}

fn main() {
    let now = at(21, 32, 8);
    let out = Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/images");
    std::fs::create_dir_all(&out).unwrap();
    let tmp = tempfile::tempdir().expect("temp dir");

    let fresh = || {
        let mut app = App::new(now);
        app.snap = Some(snapshot(now));
        app
    };

    let mut app = fresh();
    shoot("dashboard", 21, &out, tmp.path(), &mut app);
    let mut app = fresh();
    app.selected = 2;
    shoot("dashboard-fault", 21, &out, tmp.path(), &mut app);

    let mut app = fresh();
    app.mode = Mode::Help;
    shoot("help", 25, &out, tmp.path(), &mut app);

    let log = tmp.path().join("eink-weather.log");
    std::fs::write(&log, DEMO_LOG).unwrap();
    let mut app = fresh();
    app.log = LogState::open(Some(log));
    app.mode = Mode::Log("eink-weather".into());
    shoot("log", 24, &out, tmp.path(), &mut app);

    let mut app = fresh();
    app.view = View::Services;
    app.stacks = Some(stacks());
    app.pending.insert("homelab/sonarr".into(), (PendingKind::Stopping, Instant::now()));
    shoot("services", 30, &out, tmp.path(), &mut app);
}
