use crate::config::{Job, Schedule};
use crate::paths::Paths;
use crate::state::{read_paused, set_paused};
use std::collections::BTreeSet;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

pub const LABEL_PREFIX: &str = "com.chngu.hq.";

#[derive(Debug)]
pub struct CmdOutput {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

pub trait Launchctl: Send + Sync {
    fn exec(&self, args: &[&str]) -> CmdOutput;
}

pub struct RealLaunchctl;

impl Launchctl for RealLaunchctl {
    fn exec(&self, args: &[&str]) -> CmdOutput {
        match std::process::Command::new("launchctl").args(args).output() {
            Ok(o) => CmdOutput {
                code: o.status.code().unwrap_or(-1),
                stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
            },
            Err(e) => CmdOutput { code: -1, stdout: String::new(), stderr: e.to_string() },
        }
    }
}

/// HQ_FAKE_LAUNCHCTL=1: record calls instead of touching launchd (tests and dry runs).
pub struct FakeLaunchctl {
    pub paths: Paths,
}

impl Launchctl for FakeLaunchctl {
    fn exec(&self, args: &[&str]) -> CmdOutput {
        std::fs::create_dir_all(&self.paths.home).expect("fake launchctl: cannot create HQ_HOME");
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.paths.launchctl_calls())
            .expect("fake launchctl: cannot open launchctl.calls");
        writeln!(f, "{}", args.join(" ")).expect("fake launchctl: cannot append to launchctl.calls");
        if args.first() == Some(&"print") {
            let id = args.get(1).and_then(|t| t.rsplit('/').next()).unwrap_or("");
            return if self.paths.agents.join(format!("{id}.plist")).exists() {
                CmdOutput { code: 0, stdout: "\tstate = not running\n".into(), stderr: String::new() }
            } else {
                CmdOutput { code: 113, stdout: String::new(), stderr: "Could not find service".into() }
            };
        }
        CmdOutput { code: 0, stdout: String::new(), stderr: String::new() }
    }
}

pub fn launchctl_from_env(paths: &Paths) -> Box<dyn Launchctl> {
    if std::env::var("HQ_FAKE_LAUNCHCTL").as_deref() == Ok("1") {
        Box::new(FakeLaunchctl { paths: paths.clone() })
    } else {
        Box::new(RealLaunchctl)
    }
}

pub fn label(id: &str) -> String {
    format!("{LABEL_PREFIX}{id}")
}
fn domain() -> String {
    format!("gui/{}", unsafe { libc::getuid() })
}
fn target(id: &str) -> String {
    format!("{}/{}", domain(), label(id))
}
fn plist_path(paths: &Paths, id: &str) -> PathBuf {
    paths.agents.join(format!("{}.plist", label(id)))
}

pub struct PlistCtx {
    pub bin: PathBuf,
    pub hq_home: Option<PathBuf>,
}

impl PlistCtx {
    pub fn current(paths: &Paths) -> PlistCtx {
        PlistCtx {
            bin: std::env::current_exe().unwrap_or_else(|_| PathBuf::from("hq")),
            hq_home: std::env::var_os("HQ_HOME").filter(|v| !v.is_empty()).map(|_| paths.home.clone()),
        }
    }
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// Panics on a manual job: callers filter those out (they have no LaunchAgent).
pub fn render_plist(job: &Job, ctx: &PlistCtx, paths: &Paths) -> String {
    assert!(job.schedule != Schedule::Manual, "{} is manual: it has no LaunchAgent", job.id);
    let launchd_log = match &ctx.hq_home {
        Some(h) => h.join("logs").join("_launchd.log"),
        None => paths.launchd_log(),
    };
    let log = esc(&launchd_log.to_string_lossy());
    let mut l: Vec<String> = vec![
        r#"<?xml version="1.0" encoding="UTF-8"?>"#.into(),
        r#"<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">"#.into(),
        r#"<plist version="1.0">"#.into(),
        "<dict>".into(),
        "  <key>Label</key>".into(),
        format!("  <string>{}</string>", esc(&label(&job.id))),
        "  <key>ProgramArguments</key>".into(),
        "  <array>".into(),
    ];
    for a in [ctx.bin.to_string_lossy().as_ref(), "run", job.id.as_str()] {
        l.push(format!("    <string>{}</string>", esc(a)));
    }
    l.push("  </array>".into());
    match &job.schedule {
        Schedule::Every { seconds } => {
            l.push("  <key>StartInterval</key>".into());
            l.push(format!("  <integer>{seconds}</integer>"));
        }
        Schedule::At { times } => {
            l.push("  <key>StartCalendarInterval</key>".into());
            l.push("  <array>".into());
            for (hour, minute) in times {
                l.push("    <dict>".into());
                l.push("      <key>Hour</key>".into());
                l.push(format!("      <integer>{hour}</integer>"));
                l.push("      <key>Minute</key>".into());
                l.push(format!("      <integer>{minute}</integer>"));
                l.push("    </dict>".into());
            }
            l.push("  </array>".into());
        }
        Schedule::Manual => unreachable!(),
    }
    l.extend([
        "  <key>RunAtLoad</key>".into(),
        "  <false/>".into(),
        "  <key>StandardOutPath</key>".into(),
        format!("  <string>{log}</string>"),
        "  <key>StandardErrorPath</key>".into(),
        format!("  <string>{log}</string>"),
    ]);
    if let Some(h) = &ctx.hq_home {
        l.extend([
            "  <key>EnvironmentVariables</key>".into(),
            "  <dict>".into(),
            "    <key>HQ_HOME</key>".into(),
            format!("    <string>{}</string>", esc(&h.to_string_lossy())),
            "  </dict>".into(),
        ]);
    }
    l.push("</dict>".into());
    l.push("</plist>".into());
    l.push(String::new());
    l.join("\n")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentState {
    pub linked: bool,
    pub running: bool,
    pub last_exit: Option<i32>,
}

pub fn agent_state(id: &str, lc: &dyn Launchctl) -> AgentState {
    let r = lc.exec(&["print", &target(id)]);
    if r.code != 0 {
        return AgentState { linked: false, running: false, last_exit: None };
    }
    // Only top-level properties (exactly one tab deep); nested blocks repeat keys like "state".
    let mut running = false;
    let mut last_exit = None;
    for line in r.stdout.lines() {
        if let Some(v) = line.strip_prefix("\tstate = ") {
            running = v.trim() == "running";
        } else if let Some(v) = line.strip_prefix("\tlast exit code = ") {
            let digits = v.strip_prefix('-').unwrap_or(v);
            let n = digits.chars().take_while(|c| c.is_ascii_digit()).count();
            if n > 0 {
                let end = v.len() - digits.len() + n;
                last_exit = v[..end].parse().ok();
            }
        }
    }
    AgentState { linked: true, running, last_exit }
}

/// Ids of installed hq LaunchAgents, from the agents dir, sorted.
/// A missing dir is empty; one that exists but cannot be read (e.g. a file in its place) is an error, as in TS.
fn installed_ids(paths: &Paths) -> io::Result<Vec<String>> {
    if !paths.agents.exists() {
        return Ok(vec![]);
    }
    let rd = std::fs::read_dir(&paths.agents)?;
    let mut ids: Vec<String> = rd
        .filter_map(|e| e.ok())
        .filter_map(|e| e.file_name().into_string().ok())
        .filter_map(|f| f.strip_prefix(LABEL_PREFIX)?.strip_suffix(".plist").map(String::from))
        .collect();
    ids.sort();
    Ok(ids)
}

#[derive(Debug, PartialEq, Eq)]
pub struct SyncPlan {
    pub add: Vec<String>,
    pub change: Vec<String>,
    pub remove: Vec<String>,
}

pub fn plan_sync(all: &[Job], ctx: &PlistCtx, paths: &Paths) -> io::Result<SyncPlan> {
    let jobs: Vec<&Job> = all.iter().filter(|j| j.schedule != Schedule::Manual).collect(); // on-demand jobs have no agent
    let installed: BTreeSet<String> = installed_ids(paths)?.into_iter().collect();
    let mut plan = SyncPlan { add: vec![], change: vec![], remove: vec![] };
    for job in &jobs {
        if !installed.contains(&job.id) {
            plan.add.push(job.id.clone());
        } else if std::fs::read_to_string(plist_path(paths, &job.id))? != render_plist(job, ctx, paths) {
            plan.change.push(job.id.clone());
        }
    }
    let wanted: BTreeSet<&str> = jobs.iter().map(|j| j.id.as_str()).collect();
    plan.remove = installed.into_iter().filter(|id| !wanted.contains(id.as_str())).collect();
    Ok(plan)
}

fn bootstrap(path: &Path, lc: &dyn Launchctl) -> Result<(), String> {
    let p = path.to_string_lossy();
    let r = lc.exec(&["bootstrap", &domain(), &p]);
    if r.code != 0 {
        return Err(format!("launchctl bootstrap {p} failed ({}): {}", r.code, r.stderr.trim()));
    }
    Ok(())
}

fn io_err(e: io::Error) -> String {
    e.to_string()
}

pub fn apply_sync(plan: &SyncPlan, jobs: &[Job], ctx: &PlistCtx, paths: &Paths, lc: &dyn Launchctl) -> Result<(), String> {
    std::fs::create_dir_all(&paths.agents).map_err(io_err)?;
    let held = read_paused(paths);
    for id in plan.add.iter().chain(plan.change.iter()) {
        let job = jobs.iter().find(|j| &j.id == id).ok_or_else(|| format!("sync plan names unknown job {id}"))?;
        let path = plist_path(paths, id);
        std::fs::write(&path, render_plist(job, ctx, paths)).map_err(io_err)?;
        lc.exec(&["bootout", &target(id)]); // ignore failure: it may not be loaded
        if !held.contains(id) {
            bootstrap(&path, lc)?;
        }
    }
    for id in &plan.remove {
        lc.exec(&["bootout", &target(id)]);
        std::fs::remove_file(plist_path(paths, id)).map_err(io_err)?;
    }
    Ok(())
}

pub fn hold(id: &str, paths: &Paths, lc: &dyn Launchctl) -> io::Result<()> {
    lc.exec(&["bootout", &target(id)]);
    set_paused(paths, id, true)
}

pub fn resume(job: &Job, ctx: &PlistCtx, paths: &Paths, lc: &dyn Launchctl) -> Result<(), String> {
    if job.schedule == Schedule::Manual {
        return set_paused(paths, &job.id, false).map_err(io_err);
    }
    std::fs::create_dir_all(&paths.agents).map_err(io_err)?;
    let path = plist_path(paths, &job.id);
    if !path.exists() {
        std::fs::write(&path, render_plist(job, ctx, paths)).map_err(io_err)?;
    }
    bootstrap(&path, lc)?;
    set_paused(paths, &job.id, false).map_err(io_err)
}

pub fn uninstall(paths: &Paths, lc: &dyn Launchctl) -> Result<(), String> {
    for id in installed_ids(paths).map_err(io_err)? {
        lc.exec(&["bootout", &target(&id)]);
        std::fs::remove_file(plist_path(paths, &id)).map_err(io_err)?;
    }
    let Ok(rd) = std::fs::read_dir(&paths.migrated) else {
        return Ok(());
    };
    let mut files: Vec<String> = rd
        .filter_map(|e| e.ok())
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|f| f.ends_with(".plist"))
        .collect();
    files.sort();
    for f in files {
        let dest = paths.agents.join(&f);
        std::fs::copy(paths.migrated.join(&f), &dest).map_err(io_err)?;
        bootstrap(&dest, lc)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Job, Schedule};
    use crate::state::{read_paused, set_paused};
    use std::path::PathBuf;

    const FIXTURE: &str = "gui/501/com.chngu.hq.demo = {\n\tactive count = 0\n\tpath = /Users/x/Library/LaunchAgents/com.chngu.hq.demo.plist\n\ttype = LaunchAgent\n\tstate = not running\n\n\tprogram = /usr/local/bin/bun\n\tdomain = gui/501 [100033]\n\tminimum runtime = 10\n\texit timeout = 5\n\truns = 73\n\tlast exit code = 0\n\n\tresource coalition = {\n\t\tID = 1298\n\t\ttype = resource\n\t\tstate = active\n\t\tactive count = 1\n\t\tname = com.chngu.hq.demo\n\t}\n}\n";

    fn ctx() -> PlistCtx {
        PlistCtx { bin: PathBuf::from("/opt/hq"), hq_home: Some(PathBuf::from("/hqhome")) }
    }
    fn every() -> Job {
        Job {
            id: "eink".into(),
            description: String::new(),
            command: "x".into(),
            cwd: PathBuf::from("/"),
            schedule: Schedule::Every { seconds: 1800 },
            quiet: None,
            timeout_sec: 60,
            url: None,
        }
    }
    fn at() -> Job {
        Job { id: "wipe".into(), schedule: Schedule::At { times: vec![(6, 55), (19, 30)] }, ..every() }
    }
    fn manual() -> Job {
        Job { id: "tool".into(), schedule: Schedule::Manual, ..every() }
    }
    fn setup() -> (tempfile::TempDir, Paths) {
        let d = tempfile::tempdir().unwrap();
        let p = Paths::temp(d.path());
        std::fs::create_dir_all(&p.agents).unwrap();
        (d, p)
    }
    fn plist_file(p: &Paths, id: &str) -> PathBuf {
        p.agents.join(format!("{}.plist", label(id)))
    }
    fn uid() -> u32 {
        unsafe { libc::getuid() }
    }
    fn calls(p: &Paths) -> Vec<String> {
        std::fs::read_to_string(p.launchctl_calls()).unwrap_or_default().lines().map(String::from).collect()
    }

    #[test]
    fn plist_every() {
        let (_d, p) = setup();
        let want = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>
<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">
<plist version=\"1.0\">
<dict>
  <key>Label</key>
  <string>com.chngu.hq.eink</string>
  <key>ProgramArguments</key>
  <array>
    <string>/opt/hq</string>
    <string>run</string>
    <string>eink</string>
  </array>
  <key>StartInterval</key>
  <integer>1800</integer>
  <key>RunAtLoad</key>
  <false/>
  <key>StandardOutPath</key>
  <string>/hqhome/logs/_launchd.log</string>
  <key>StandardErrorPath</key>
  <string>/hqhome/logs/_launchd.log</string>
  <key>EnvironmentVariables</key>
  <dict>
    <key>HQ_HOME</key>
    <string>/hqhome</string>
  </dict>
</dict>
</plist>
";
        assert_eq!(render_plist(&every(), &ctx(), &p), want);
    }

    #[test]
    fn plist_at() {
        let (_d, p) = setup();
        let want = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>
<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">
<plist version=\"1.0\">
<dict>
  <key>Label</key>
  <string>com.chngu.hq.wipe</string>
  <key>ProgramArguments</key>
  <array>
    <string>/opt/hq</string>
    <string>run</string>
    <string>wipe</string>
  </array>
  <key>StartCalendarInterval</key>
  <array>
    <dict>
      <key>Hour</key>
      <integer>6</integer>
      <key>Minute</key>
      <integer>55</integer>
    </dict>
    <dict>
      <key>Hour</key>
      <integer>19</integer>
      <key>Minute</key>
      <integer>30</integer>
    </dict>
  </array>
  <key>RunAtLoad</key>
  <false/>
  <key>StandardOutPath</key>
  <string>/hqhome/logs/_launchd.log</string>
  <key>StandardErrorPath</key>
  <string>/hqhome/logs/_launchd.log</string>
  <key>EnvironmentVariables</key>
  <dict>
    <key>HQ_HOME</key>
    <string>/hqhome</string>
  </dict>
</dict>
</plist>
";
        let xml = render_plist(&at(), &ctx(), &p);
        assert_eq!(xml, want);
        assert!(!xml.contains("StartInterval"));
    }

    #[test]
    fn plist_without_hq_home_uses_paths_home_and_no_env() {
        let (_d, p) = setup();
        let c = PlistCtx { bin: PathBuf::from("/opt/hq"), hq_home: None };
        let xml = render_plist(&every(), &c, &p);
        assert!(xml.contains(&format!("<string>{}</string>", p.home.join("logs/_launchd.log").display())));
        assert!(!xml.contains("EnvironmentVariables"));
    }

    #[test]
    fn plist_escapes_xml() {
        let (_d, p) = setup();
        let c = PlistCtx { bin: PathBuf::from("/a&b/<c>/hq"), hq_home: None };
        assert!(render_plist(&every(), &c, &p).contains("/a&amp;b/&lt;c&gt;/hq"));
    }

    #[test]
    #[should_panic(expected = "manual")]
    fn plist_panics_for_manual() {
        let (_d, p) = setup();
        render_plist(&manual(), &ctx(), &p);
    }

    #[test]
    fn plan_empty_dir_adds_all() {
        let (_d, p) = setup();
        let plan = plan_sync(&[every(), at()], &ctx(), &p).unwrap();
        assert_eq!(plan, SyncPlan { add: vec!["eink".into(), "wipe".into()], change: vec![], remove: vec![] });
    }

    #[test]
    fn plan_manual_gets_no_agent_and_stale_one_removed() {
        let (_d, p) = setup();
        let plan = plan_sync(&[every(), manual()], &ctx(), &p).unwrap();
        assert_eq!(plan, SyncPlan { add: vec!["eink".into()], change: vec![], remove: vec![] });
        std::fs::write(plist_file(&p, "tool"), "<plist/>").unwrap();
        assert_eq!(plan_sync(&[every(), manual()], &ctx(), &p).unwrap().remove, ["tool"]);
    }

    #[test]
    fn plan_detects_change_removal_and_ignores_foreign() {
        let (_d, p) = setup();
        let stale = Job { schedule: Schedule::Every { seconds: 60 }, ..every() };
        std::fs::write(plist_file(&p, "eink"), render_plist(&stale, &ctx(), &p)).unwrap();
        std::fs::write(plist_file(&p, "wipe"), render_plist(&at(), &ctx(), &p)).unwrap();
        std::fs::write(plist_file(&p, "old"), "<plist/>").unwrap();
        std::fs::write(p.agents.join("com.chngu.trml.x.plist"), "<plist/>").unwrap();
        let plan = plan_sync(&[every(), at()], &ctx(), &p).unwrap();
        assert_eq!(plan, SyncPlan { add: vec![], change: vec!["eink".into()], remove: vec!["old".into()] });
    }

    #[test]
    fn apply_sync_bootout_bootstrap_remove_and_held() {
        let (_d, p) = setup();
        std::fs::write(plist_file(&p, "old"), "<plist/>").unwrap();
        set_paused(&p, "wipe", true).unwrap();
        let lc = FakeLaunchctl { paths: p.clone() };
        let plan = SyncPlan { add: vec!["wipe".into()], change: vec!["eink".into()], remove: vec!["old".into()] };
        apply_sync(&plan, &[every(), at()], &ctx(), &p, &lc).unwrap();
        let u = uid();
        assert_eq!(
            calls(&p),
            vec![
                format!("bootout gui/{u}/{}", label("wipe")),
                format!("bootout gui/{u}/{}", label("eink")),
                format!("bootstrap gui/{u} {}", plist_file(&p, "eink").display()),
                format!("bootout gui/{u}/{}", label("old")),
            ]
        );
        assert_eq!(std::fs::read_to_string(plist_file(&p, "wipe")).unwrap(), render_plist(&at(), &ctx(), &p));
        assert!(!plist_file(&p, "old").exists());
    }

    #[test]
    fn agent_state_parses_top_level() {
        let lc = Canned { code: 0, stdout: FIXTURE.into() };
        assert_eq!(agent_state("demo", &lc), AgentState { linked: true, running: false, last_exit: Some(0) });
    }

    #[test]
    fn agent_state_running() {
        let out = FIXTURE.replace("state = not running", "state = running").replace("last exit code = 0", "last exit code = (never exited)");
        let lc = Canned { code: 0, stdout: out };
        assert_eq!(agent_state("demo", &lc), AgentState { linked: true, running: true, last_exit: None });
    }

    #[test]
    fn agent_state_missing_service_is_unlinked() {
        let lc = Canned { code: 113, stdout: String::new() };
        assert_eq!(agent_state("demo", &lc), AgentState { linked: false, running: false, last_exit: None });
    }

    struct Canned {
        code: i32,
        stdout: String,
    }
    impl Launchctl for Canned {
        fn exec(&self, _args: &[&str]) -> CmdOutput {
            CmdOutput { code: self.code, stdout: self.stdout.clone(), stderr: String::new() }
        }
    }

    #[test]
    fn hold_then_resume() {
        let (_d, p) = setup();
        let lc = FakeLaunchctl { paths: p.clone() };
        hold("eink", &p, &lc).unwrap();
        assert!(read_paused(&p).contains("eink"));
        resume(&every(), &ctx(), &p, &lc).unwrap();
        assert!(!read_paused(&p).contains("eink"));
        assert!(plist_file(&p, "eink").exists());
        let u = uid();
        assert_eq!(
            calls(&p),
            vec![format!("bootout gui/{u}/{}", label("eink")), format!("bootstrap gui/{u} {}", plist_file(&p, "eink").display())]
        );
    }

    #[test]
    fn resume_manual_only_clears_pause() {
        let (_d, p) = setup();
        set_paused(&p, "tool", true).unwrap();
        let lc = FakeLaunchctl { paths: p.clone() };
        resume(&manual(), &ctx(), &p, &lc).unwrap();
        assert!(!read_paused(&p).contains("tool"));
        assert!(calls(&p).is_empty());
    }

    #[test]
    fn uninstall_removes_hq_agents_and_restores_migrated() {
        let (_d, p) = setup();
        std::fs::write(plist_file(&p, "eink"), "<plist/>").unwrap();
        std::fs::create_dir_all(&p.migrated).unwrap();
        std::fs::write(p.migrated.join("com.chngu.trml.reminders.plist"), "<plist>orig</plist>").unwrap();
        let lc = FakeLaunchctl { paths: p.clone() };
        uninstall(&p, &lc).unwrap();
        let restored = p.agents.join("com.chngu.trml.reminders.plist");
        assert!(!plist_file(&p, "eink").exists());
        assert_eq!(std::fs::read_to_string(&restored).unwrap(), "<plist>orig</plist>");
        let u = uid();
        assert_eq!(
            calls(&p),
            vec![format!("bootout gui/{u}/{}", label("eink")), format!("bootstrap gui/{u} {}", restored.display())]
        );
    }

    #[test]
    fn fake_print_reflects_plist_presence() {
        let (_d, p) = setup();
        let lc = FakeLaunchctl { paths: p.clone() };
        assert!(!agent_state("eink", &lc).linked);
        std::fs::write(plist_file(&p, "eink"), "<plist/>").unwrap();
        assert_eq!(agent_state("eink", &lc), AgentState { linked: true, running: false, last_exit: None });
    }
}
