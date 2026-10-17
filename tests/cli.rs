use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::Duration;

struct Fx {
    _dir: tempfile::TempDir,
    root: PathBuf,
}

impl Fx {
    fn new() -> Fx {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let fx = Fx { _dir: dir, root };
        fx.config("[jobs.ok]\ncommand = \"echo fine\"\nevery = \"5m\"\n\n[jobs.bad]\ncommand = \"exit 4\"\nevery = \"1h\"\n");
        fx
    }
    fn config(&self, text: &str) {
        std::fs::write(self.root.join("jobs.toml"), text).unwrap();
    }
    fn home(&self) -> PathBuf {
        self.root.join("home")
    }
    fn agents(&self) -> PathBuf {
        self.root.join("agents")
    }
    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_hq"));
        c.args(args)
            .env("HQ_HOME", self.home())
            .env("HQ_CONFIG", self.root.join("jobs.toml"))
            .env("HQ_LAUNCH_AGENTS_DIR", self.agents())
            .env("HQ_MIGRATED_DIR", self.root.join("migrated"))
            .env("HQ_FAKE_LAUNCHCTL", "1")
            .stdin(Stdio::null());
        c
    }
    fn hq(&self, args: &[&str]) -> Res {
        Res(self.cmd(args).output().unwrap())
    }
}

struct Res(Output);
impl Res {
    fn code(&self) -> i32 {
        self.0.status.code().unwrap()
    }
    fn out(&self) -> String {
        String::from_utf8_lossy(&self.0.stdout).into_owned()
    }
    fn err(&self) -> String {
        String::from_utf8_lossy(&self.0.stderr).into_owned()
    }
}

fn line_matches(text: &str, id: &str, word: &str) -> bool {
    text.lines().any(|l| {
        let f: Vec<&str> = l.split_whitespace().collect();
        f.len() >= 5 && f[0] == id && f[1] == word && f[2] == "last"
    })
}

fn json(fx: &Fx) -> Vec<Value> {
    let r = fx.hq(&["status", "--json"]);
    assert_eq!(r.code(), 0, "{}", r.err());
    serde_json::from_str::<Value>(&r.out()).unwrap().as_array().unwrap().clone()
}

#[test]
fn status_json_lists_jobs() {
    let fx = Fx::new();
    let v = json(&fx);
    let ids: Vec<&str> = v.iter().map(|j| j["job"]["id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["bad", "ok"]);
    let j = &v[0];
    for k in ["job", "held", "agent", "synced", "last", "recent", "running", "next"] {
        assert!(j.get(k).is_some(), "missing {k}");
    }
    assert_eq!(j["job"]["schedule"], serde_json::json!({"kind": "every", "seconds": 3600}));
    assert_eq!(j["job"]["timeoutSec"], 600);
    assert!(j["job"]["quiet"].is_null() && j["job"]["url"].is_null() && j["last"].is_null());
    assert_eq!(j["agent"], serde_json::json!({"linked": false, "running": false, "lastExit": null}));
    assert!(j["next"].as_str().unwrap().ends_with('Z'));
}

#[test]
fn json_rows_are_camel_case() {
    let fx = Fx::new();
    fx.hq(&["run", "bad", "--manual"]);
    let v = json(&fx);
    let last = &v[0]["last"];
    for k in ["id", "job", "trigger", "startedAt", "endedAt", "exitCode", "status", "reason", "tail"] {
        assert!(last.get(k).is_some(), "missing {k}");
    }
    assert_eq!(last["exitCode"], 4);
    assert_eq!(last["status"], "failed");
    assert_eq!(v[0]["recent"].as_array().unwrap().len(), 1);
}

#[test]
fn run_manual_returns_exit_code_and_shows_in_status() {
    let fx = Fx::new();
    assert_eq!(fx.hq(&["run", "bad", "--manual"]).code(), 4);
    assert_eq!(fx.hq(&["run", "ok", "--manual"]).code(), 0);
    let s = fx.hq(&["status"]).out();
    assert!(line_matches(&s, "bad", "FAULT"), "{s}");
    assert!(line_matches(&s, "ok", "OK"), "{s}");
    assert!(s.lines().all(|l| l.contains("next ")), "{s}");
}

#[test]
fn run_unknown_job_exits_2() {
    let fx = Fx::new();
    let r = fx.hq(&["run", "nope"]);
    assert_eq!(r.code(), 2);
    assert!(r.err().contains("unknown job nope"), "{}", r.err());
}

#[test]
fn hold_and_resume_toggle_hold() {
    let fx = Fx::new();
    assert_eq!(fx.hq(&["hold", "ok"]).out(), "ok on hold\n");
    assert!(line_matches(&fx.hq(&["status"]).out(), "ok", "HOLD"));
    assert_eq!(fx.hq(&["resume", "ok"]).out(), "ok resumed\n");
    assert!(line_matches(&fx.hq(&["status"]).out(), "ok", "NEVER"));
}

#[test]
fn sync_yes_writes_plists_and_calls_launchctl() {
    let fx = Fx::new();
    let r = fx.hq(&["sync", "--yes"]);
    assert_eq!(r.code(), 0, "{}", r.err());
    assert!(r.out().contains("+ bad"));
    assert!(r.out().ends_with("synced\n"));
    assert!(fx.agents().join("com.chngu.hq.ok.plist").exists());
    assert!(std::fs::read_to_string(fx.home().join("launchctl.calls")).unwrap().contains("bootstrap"));
    assert!(fx.hq(&["sync", "--yes"]).out().contains("in sync"));
    let v = json(&fx);
    assert!(v.iter().all(|j| j["synced"] == true));
}

#[test]
fn logs_prints_the_job_log() {
    let fx = Fx::new();
    fx.hq(&["run", "ok", "--manual"]);
    let r = fx.hq(&["logs", "ok"]);
    assert!(r.out().contains("fine"));
    assert!(r.out().contains("── RUN 0001"));
}

#[test]
fn sigterm_to_a_running_job_finishes_the_run_and_frees_the_lock() {
    let fx = Fx::new();
    let pid_file = fx.root.join("pid");
    fx.config(&format!("[jobs.slow]\ncommand = \"sleep 20 & echo $! > {}; wait\"\nevery = \"5m\"\n", pid_file.display()));
    let mut child = fx.cmd(&["run", "slow", "--manual"]).stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();
    for _ in 0..100 {
        if std::fs::read_to_string(&pid_file).map(|s| !s.trim().is_empty()).unwrap_or(false) {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let pid: i32 = std::fs::read_to_string(&pid_file).unwrap().trim().parse().unwrap();
    unsafe { libc::kill(child.id() as i32, libc::SIGTERM) };
    child.wait().unwrap();
    std::thread::sleep(Duration::from_millis(100));
    assert_ne!(unsafe { libc::kill(pid, 0) }, 0, "job process still alive");
    assert!(!fx.home().join("locks/slow.lock").exists());
    let v = json(&fx);
    assert_eq!(v[0]["last"]["status"], "failed");
    assert!(v[0]["last"]["tail"].as_str().unwrap().contains("killed by SIGTERM"));
    let log = std::fs::read_to_string(fx.home().join("logs/slow.log")).unwrap();
    assert!(log.contains(" · FAULT · "), "{log}");
}

#[test]
fn run_with_a_broken_config_records_a_fault() {
    let fx = Fx::new();
    fx.config("[jobs.ok]\ncolour = 1\n");
    let r = fx.hq(&["run", "ok"]);
    assert_eq!(r.code(), 1);
    assert!(r.err().starts_with("hq: "), "{}", r.err());
    let log = std::fs::read_to_string(fx.home().join("logs/ok.log")).unwrap();
    assert!(log.starts_with("hq: config error: ") && log.contains("jobs.ok"), "{log}");
    // The config is broken, so read the row straight from the database.
    let h = Path::new(&fx.home()).join("history.db");
    let c = rusqlite::Connection::open(h).unwrap();
    let (status, tail): (String, String) =
        c.query_row("SELECT status, tail FROM runs WHERE job = 'ok'", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
    assert_eq!(status, "failed");
    assert!(tail.contains("config error"));
}

#[test]
fn broken_config_is_reported_not_a_panic() {
    let fx = Fx::new();
    fx.config("[jobs.x]\ncolour = 1\n");
    let r = fx.hq(&["status"]);
    assert_eq!(r.code(), 1);
    assert!(r.err().contains("hq: jobs.x"), "{}", r.err());
    assert!(!r.err().contains("panicked"));
}

#[test]
fn usage_and_no_command() {
    let fx = Fx::new();
    let r = fx.hq(&["bogus"]);
    assert_eq!(r.code(), 2);
    assert!(r.err().starts_with("usage: hq [status [--json]"), "{}", r.err());
    let r = fx.hq(&[]);
    assert_eq!(r.code(), 0);
    assert_eq!(r.out(), "tui: not yet\n");
}

#[test]
fn uninstall_removes_agents() {
    let fx = Fx::new();
    fx.hq(&["sync", "--yes"]);
    let r = fx.hq(&["uninstall"]);
    assert_eq!(r.code(), 0, "{}", r.err());
    assert_eq!(r.out(), "removed hq agents; restored migrated/ plists\n");
    assert!(!fx.agents().join("com.chngu.hq.ok.plist").exists());
}

/// A directory where history.db should be makes History::open fail.
fn break_history(fx: &Fx) {
    std::fs::create_dir_all(fx.home().join("history.db")).unwrap();
}

#[test]
fn run_survives_unavailable_history() {
    let fx = Fx::new();
    fx.config("[jobs.ok]\ncommand = \"exit 0\"\nevery = \"5m\"\n\n[jobs.three]\ncommand = \"exit 3\"\nevery = \"5m\"\n");
    break_history(&fx);
    for (id, code) in [("ok", 0), ("three", 3)] {
        let r = fx.hq(&["run", id, "--manual"]);
        assert_eq!(r.code(), code, "{}", r.err());
        assert!(!r.err().contains("panicked"), "{}", r.err());
        let log = std::fs::read_to_string(fx.home().join(format!("logs/{id}.log"))).unwrap();
        assert!(log.contains("hq: history unavailable: "), "{log}");
        assert!(log.contains("── END "), "{log}");
    }
}

#[test]
fn status_reports_unavailable_history() {
    let fx = Fx::new();
    break_history(&fx);
    for args in [&["status"][..], &["status", "--json"][..]] {
        let r = fx.hq(args);
        assert_eq!(r.code(), 1);
        assert!(r.err().contains("hq: history unavailable: "), "{}", r.err());
        assert!(r.out().contains("ok"), "stdout still printed: {}", r.out());
    }
}

#[test]
fn status_survives_a_closed_pipe() {
    let fx = Fx::new();
    let mut child = fx.cmd(&["status"]).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    drop(child.stdout.take());
    let out = child.wait_with_output().unwrap();
    assert!(!String::from_utf8_lossy(&out.stderr).contains("panicked"));
}
