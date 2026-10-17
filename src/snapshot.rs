use crate::config::{load_config, Job, Schedule};
use crate::history::{History, RunRow, Status};
use crate::launchd::{agent_state, label, render_plist, AgentState, Launchctl, PlistCtx};
use crate::paths::Paths;
use crate::schedule::next_run;
use crate::state::read_paused;
use chrono::{DateTime, Local, TimeZone, Utc};
use serde_json::{json, Value};
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct JobView {
    pub job: Job,
    pub held: bool,
    pub agent: AgentState,
    pub synced: bool,
    /// Newest non-skipped run.
    pub last: Option<RunRow>,
    /// Newest first, up to 10, including skips.
    pub recent: Vec<RunRow>,
    pub running: bool,
    pub next: Option<DateTime<Local>>,
}

#[derive(Debug, Clone)]
pub struct Snapshot {
    pub jobs: Vec<JobView>,
    pub config_error: Option<String>,
    pub at: DateTime<Local>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Summary {
    pub systems: usize,
    pub nominal: usize,
    pub fault: usize,
    pub standby: usize,
}

/// `launchctl print` for every job, in parallel (it is a process spawn each).
pub fn fetch_agents(jobs: &[Job], lc: &dyn Launchctl) -> HashMap<String, AgentState> {
    std::thread::scope(|s| {
        let handles: Vec<_> = jobs
            .iter()
            .map(|j| (j.id.clone(), s.spawn(move || agent_state(&j.id, lc))))
            .collect();
        handles
            .into_iter()
            .map(|(id, h)| {
                let st = h.join().unwrap_or(AgentState { linked: false, running: false, last_exit: None });
                (id, st)
            })
            .collect()
    })
}

pub fn load_snapshot(
    paths: &Paths,
    history: &History,
    agents: &HashMap<String, AgentState>,
    now: DateTime<Local>,
    previous: Option<&Snapshot>,
) -> Snapshot {
    let (jobs, config_error) = match load_config(&paths.config) {
        Ok(j) => (j, None),
        Err(e) => (previous.map(|p| p.jobs.iter().map(|v| v.job.clone()).collect()).unwrap_or_default(), Some(e.0)),
    };
    let held = read_paused(paths);
    let ctx = PlistCtx::current(paths);
    let views = jobs
        .into_iter()
        .map(|job| {
            let agent = agents.get(&job.id).cloned().unwrap_or(AgentState { linked: false, running: false, last_exit: None });
            let is_held = held.contains(&job.id);
            let manual = job.schedule == Schedule::Manual;
            let plist_matches = manual || {
                let path = paths.agents.join(format!("{}.plist", label(&job.id)));
                std::fs::read_to_string(path).map(|t| t == render_plist(&job, &ctx, paths)).unwrap_or(false)
            };
            let recent = history.recent(&job.id, 10).unwrap_or_default();
            let last_start = recent.first().and_then(|r| Local.timestamp_millis_opt(r.started_at).single());
            JobView {
                held: is_held,
                synced: manual || (plist_matches && (agent.linked || is_held)),
                last: history.last_real(&job.id).unwrap_or(None),
                running: recent.first().is_some_and(|r| r.status == Status::Running),
                next: if is_held { None } else { next_run(&job, last_start, now) },
                agent,
                recent,
                job,
            }
        })
        .collect();
    Snapshot { jobs: views, config_error, at: now }
}

pub fn summarize(s: &Snapshot) -> Summary {
    let mut out = Summary { systems: s.jobs.len(), nominal: 0, fault: 0, standby: 0 };
    for v in &s.jobs {
        match &v.last {
            _ if v.held => out.standby += 1,
            None => out.standby += 1,
            Some(r) if matches!(r.status, Status::Failed | Status::Timeout) => out.fault += 1,
            Some(_) => out.nominal += 1,
        }
    }
    out
}

fn run_json(r: &RunRow) -> Value {
    json!({
        "id": r.id,
        "job": r.job,
        "trigger": r.trigger.as_str(),
        "startedAt": r.started_at,
        "endedAt": r.ended_at,
        "exitCode": r.exit_code,
        "status": r.status.as_str(),
        "reason": r.reason,
        "tail": r.tail,
    })
}

impl JobView {
    /// The shape `hq status --json` has always printed (field order included).
    pub fn to_json(&self) -> Value {
        let j = &self.job;
        let schedule = match &j.schedule {
            Schedule::Every { seconds } => json!({"kind": "every", "seconds": seconds}),
            Schedule::At { times } => json!({
                "kind": "at",
                "times": times.iter().map(|(h, m)| json!({"hour": h, "minute": m})).collect::<Vec<_>>(),
            }),
            Schedule::Manual => json!({"kind": "manual"}),
        };
        json!({
            "job": {
                "id": j.id,
                "description": j.description,
                "command": j.command,
                "cwd": j.cwd.to_string_lossy(),
                "schedule": schedule,
                "quiet": j.quiet.map(|q| json!({"start": q.start, "end": q.end})),
                "timeoutSec": j.timeout_sec,
                "url": j.url,
            },
            "held": self.held,
            "agent": {"linked": self.agent.linked, "running": self.agent.running, "lastExit": self.agent.last_exit},
            "synced": self.synced,
            "last": self.last.as_ref().map(run_json),
            "recent": self.recent.iter().map(run_json).collect::<Vec<_>>(),
            "running": self.running,
            "next": self.next.map(|d| d.with_timezone(&Utc).format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::Trigger;
    use std::path::PathBuf;

    fn view(id: &str, held: bool, status: Option<Status>) -> JobView {
        JobView {
            job: Job {
                id: id.into(),
                description: String::new(),
                command: "x".into(),
                cwd: PathBuf::from("/"),
                schedule: Schedule::Every { seconds: 60 },
                quiet: None,
                timeout_sec: 60,
                url: None,
            },
            held,
            agent: AgentState { linked: true, running: false, last_exit: None },
            synced: true,
            last: status.map(|status| RunRow {
                id: 1,
                job: id.into(),
                trigger: Trigger::Manual,
                started_at: 0,
                ended_at: None,
                exit_code: None,
                status,
                reason: None,
                tail: None,
            }),
            recent: vec![],
            running: false,
            next: None,
        }
    }

    #[test]
    fn summarize_counts_by_rules() {
        let snap = Snapshot {
            at: Local::now(),
            config_error: None,
            jobs: vec![
                view("a", false, Some(Status::Ok)),
                view("b", false, Some(Status::Failed)),
                view("c", false, Some(Status::Timeout)),
                view("d", true, Some(Status::Ok)),
                view("e", false, None),
            ],
        };
        assert_eq!(summarize(&snap), Summary { systems: 5, nominal: 1, fault: 2, standby: 2 });
    }

    #[test]
    fn json_field_order_matches_ts() {
        let v = view("a", false, None).to_json();
        let keys: Vec<&String> = v.as_object().unwrap().keys().collect();
        assert_eq!(keys, ["job", "held", "agent", "synced", "last", "recent", "running", "next"]);
    }
}
