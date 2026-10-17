use crate::paths::expand_home;
use std::fmt;
use std::path::{Path, PathBuf};
use toml::{Table, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QuietWindow {
    pub start: u16, // minutes after midnight
    pub end: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Schedule {
    Every { seconds: u64 },
    At { times: Vec<(u8, u8)> },
    /// On demand only: no LaunchAgent, runs when you press R.
    Manual,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    pub id: String,
    pub description: String,
    pub command: String,
    pub cwd: PathBuf,
    pub schedule: Schedule,
    pub quiet: Option<QuietWindow>,
    pub timeout_sec: u64,
    /// Shown in the detail panel; O opens it.
    pub url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigError(pub String);

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for ConfigError {}

const KEYS: [&str; 9] = ["description", "cwd", "command", "every", "at", "manual", "quiet", "timeout", "url"];

fn err<T>(msg: impl Into<String>) -> Result<T, ConfigError> {
    Err(ConfigError(msg.into()))
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

pub fn parse_duration(s: &str) -> Result<u64, ConfigError> {
    let bad = || ConfigError(format!("invalid duration \"{s}\" (use e.g. 90s, 30m, 1h)"));
    let unit = match s.as_bytes().last() {
        Some(b's') => 1,
        Some(b'm') => 60,
        Some(b'h') => 3600,
        _ => return Err(bad()),
    };
    let digits = &s[..s.len() - 1];
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(bad());
    }
    digits.parse::<u64>().ok().and_then(|n| n.checked_mul(unit)).ok_or_else(bad)
}

fn parse_clock(s: &str) -> Result<(u8, u8), ConfigError> {
    let b = s.as_bytes();
    let ok = b.len() == 5 && b[2] == b':' && [0, 1, 3, 4].iter().all(|&i| b[i].is_ascii_digit());
    if ok {
        let (h, m) = ((b[0] - b'0') * 10 + (b[1] - b'0'), (b[3] - b'0') * 10 + (b[4] - b'0'));
        if h <= 23 && m <= 59 {
            return Ok((h, m));
        }
    }
    err(format!("invalid time \"{s}\" (use HH:MM)"))
}

pub fn parse_quiet(s: &str) -> Result<QuietWindow, ConfigError> {
    let bad = || ConfigError(format!("invalid quiet window \"{s}\" (use HH:MM-HH:MM)"));
    let parts: Vec<&str> = s.split('-').collect();
    if parts.len() != 2 {
        return Err(bad());
    }
    let a = parse_clock(parts[0].trim()).map_err(|_| bad())?;
    let b = parse_clock(parts[1].trim()).map_err(|_| bad())?;
    Ok(QuietWindow {
        start: u16::from(a.0) * 60 + u16::from(a.1),
        end: u16::from(b.0) * 60 + u16::from(b.1),
    })
}

fn is_url(s: &str) -> bool {
    let rest = s.strip_prefix("https://").or_else(|| s.strip_prefix("http://"));
    matches!(rest, Some(r) if !r.is_empty() && !r.chars().any(char::is_whitespace))
}

fn parse_job(id: &str, raw: &Table) -> Result<Job, ConfigError> {
    let fail = |msg: &str| -> ConfigError { ConfigError(format!("jobs.{id}: {msg}")) };
    if !valid_id(id) {
        return Err(fail(&format!("invalid job id \"{id}\" (use a-z, 0-9 and -)")));
    }
    for key in raw.keys() {
        if !KEYS.contains(&key.as_str()) {
            return Err(fail(&format!("unknown key {key}")));
        }
    }
    let str_of = |key: &str| -> Result<Option<String>, ConfigError> {
        match raw.get(key) {
            None => Ok(None),
            Some(Value::String(s)) => Ok(Some(s.clone())),
            Some(_) => Err(fail(&format!("{key} must be a string"))),
        }
    };
    let wrap = |key: &str, e: ConfigError| fail(&format!("{key}: {}", e.0));

    let command = match str_of("command")? {
        Some(c) if !c.is_empty() => c,
        _ => return Err(fail("command is required")),
    };

    let every = str_of("every")?;
    let at = raw.get("at");
    let manual = match raw.get("manual") {
        None => None,
        Some(Value::Boolean(b)) => Some(*b),
        Some(_) => return Err(fail("manual must be true or false")),
    };

    let schedule = if manual == Some(true) {
        if every.is_some() || at.is_some() {
            return Err(fail("a manual job has no every or at"));
        }
        if raw.contains_key("quiet") {
            return Err(fail("quiet has no effect on a manual job"));
        }
        Schedule::Manual
    } else {
        match (every, at) {
            (Some(_), Some(_)) => return Err(fail("set either every or at, not both")),
            (None, None) => return Err(fail("set every or at (neither is set), or manual = true")),
            (Some(every), None) => {
                let seconds = parse_duration(&every).map_err(|e| wrap("every", e))?;
                if seconds < 60 {
                    return Err(fail("every must be at least 60s"));
                }
                Schedule::Every { seconds }
            }
            (None, Some(at)) => {
                let strings: Option<Vec<&str>> = match at {
                    Value::Array(a) if !a.is_empty() => a.iter().map(Value::as_str).collect(),
                    _ => None,
                };
                let Some(strings) = strings else {
                    return Err(fail("at must be a list of \"HH:MM\" strings"));
                };
                let times = strings
                    .into_iter()
                    .map(|t| parse_clock(t).map_err(|e| wrap("at", e)))
                    .collect::<Result<Vec<_>, _>>()?;
                Schedule::At { times }
            }
        }
    };

    let quiet = str_of("quiet")?;
    let timeout = str_of("timeout")?;
    let url = str_of("url")?;
    if matches!(&url, Some(u) if !is_url(u)) {
        return Err(fail("url must start with http:// or https://"));
    }
    Ok(Job {
        id: id.to_string(),
        description: str_of("description")?.unwrap_or_default(),
        command,
        cwd: match str_of("cwd")? {
            Some(c) => expand_home(&c),
            None => expand_home("~"),
        },
        schedule,
        quiet: match quiet {
            Some(q) => Some(parse_quiet(&q).map_err(|e| wrap("quiet", e))?),
            None => None,
        },
        timeout_sec: match timeout {
            Some(t) => parse_duration(&t).map_err(|e| wrap("timeout", e))?,
            None => 600,
        },
        url,
    })
}

pub fn parse_config(text: &str) -> Result<Vec<Job>, ConfigError> {
    let doc: Table = text.parse().map_err(|e: toml::de::Error| ConfigError(format!("invalid TOML: {e}")))?;
    // [stacks.*] is read by the TUI only, so a mistake there never stops a job from running.
    for key in doc.keys() {
        if key != "jobs" && key != "stacks" {
            return err(format!("unknown top-level key {key}"));
        }
    }
    let mut jobs = Vec::new();
    match doc.get("jobs") {
        None => {}
        Some(Value::Table(t)) => {
            for (id, raw) in t {
                match raw {
                    Value::Table(raw) => jobs.push(parse_job(id, raw)?),
                    _ => return err(format!("jobs.{id}: must be a table")),
                }
            }
        }
        Some(_) => return err("jobs must be a table"),
    }
    // on-demand jobs first: they're launchers, the rest is the schedule
    jobs.sort_by(|a, b| {
        let manual = |j: &Job| j.schedule == Schedule::Manual;
        manual(b).cmp(&manual(a)).then_with(|| a.id.cmp(&b.id))
    });
    Ok(jobs)
}

pub fn load_config(path: &Path) -> Result<Vec<Job>, ConfigError> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| ConfigError(format!("cannot read {}: {e}", path.display())))?;
    parse_config(&text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    const SPEC_EXAMPLE: &str = r#"
[jobs.eink-weather]
description = "Punggol weather → nRF e-ink"
cwd         = "~/Developer/potateros/nrf5-eink"
command     = "uv run python -m epd.cli weather --push"
every       = "30m"
quiet       = "23:00-07:00"
timeout     = "5m"

[jobs.eink-wipe]
description = "Daily full clear of the e-ink (ghosting)"
cwd         = "~/Developer/potateros/nrf5-eink"
command     = "uv run python -m epd.cli weather --push --wipe"
at          = ["06:55"]
timeout     = "5m"

[jobs.trml-reminders]
description = "Apple Reminders → TRMNL sidecar"
command     = "~/Developer/potateros/trml-byos/host/reminders-export.sh"
every       = "5m"

[jobs.tailscale-watchdog]
description = "Restart Tailscale if it's down"
command     = "~/Developer/potateros/trml-byos/host/tailscale-watchdog.sh"
every       = "1h"
"#;

    fn one(body: &str, id: &str) -> String {
        format!("[jobs.{id}]\ncommand = \"true\"\n{body}\n")
    }

    fn err_of(text: &str) -> String {
        parse_config(text).expect_err("expected ConfigError").0
    }

    fn expect_error(text: &str, mentions: &[&str]) {
        let msg = err_of(text);
        for m in mentions {
            assert!(msg.contains(m), "{msg:?} should contain {m:?}");
        }
    }

    #[test]
    fn parses_every_job() {
        let jobs = parse_config(SPEC_EXAMPLE).unwrap();
        let ids: Vec<_> = jobs.iter().map(|j| j.id.as_str()).collect();
        assert_eq!(ids, ["eink-weather", "eink-wipe", "tailscale-watchdog", "trml-reminders"]);
        let home = std::env::var("HOME").unwrap();
        let w = jobs.iter().find(|j| j.id == "eink-weather").unwrap();
        assert_eq!(w.schedule, Schedule::Every { seconds: 1800 });
        assert_eq!(w.quiet, Some(QuietWindow { start: 1380, end: 420 }));
        assert_eq!(w.timeout_sec, 300);
        assert_eq!(w.cwd, PathBuf::from(format!("{home}/Developer/potateros/nrf5-eink")));
        assert_eq!(w.description, "Punggol weather → nRF e-ink");
        let wipe = jobs.iter().find(|j| j.id == "eink-wipe").unwrap();
        assert_eq!(wipe.schedule, Schedule::At { times: vec![(6, 55)] });
        let r = jobs.iter().find(|j| j.id == "trml-reminders").unwrap();
        assert_eq!(r.timeout_sec, 600);
        assert_eq!(r.cwd, PathBuf::from(&home));
        assert_eq!(r.quiet, None);
        assert_eq!(r.url, None);
    }

    #[test]
    fn rejects_both_every_and_at() {
        expect_error(&one("every = \"5m\"\nat = [\"07:00\"]", "both"), &["both"]);
        assert_eq!(err_of(&one("every = \"5m\"\nat = [\"07:00\"]", "x")), "jobs.x: set either every or at, not both");
    }
    #[test]
    fn rejects_neither_every_nor_at() {
        expect_error(&one("", "neither"), &["neither"]);
    }
    #[test]
    fn rejects_every_under_60s() {
        assert_eq!(err_of(&one("every = \"30s\"", "j")), "jobs.j: every must be at least 60s");
    }
    #[test]
    fn rejects_bad_duration() {
        assert_eq!(
            err_of(&one("every = \"5x\"", "j")),
            "jobs.j: every: invalid duration \"5x\" (use e.g. 90s, 30m, 1h)"
        );
    }
    #[test]
    fn rejects_bad_quiet() {
        assert_eq!(
            err_of(&one("every = \"5m\"\nquiet = \"7am-9am\"", "j")),
            "jobs.j: quiet: invalid quiet window \"7am-9am\" (use HH:MM-HH:MM)"
        );
    }
    #[test]
    fn rejects_unknown_key() {
        assert_eq!(err_of(&one("every = \"5m\"\ncolour = \"red\"", "j")), "jobs.j: unknown key colour");
    }
    #[test]
    fn first_error_in_file_order() {
        let text = "[jobs.j]\ncommand = \"true\"\nevery = \"5m\"\nzeta = 1\nalpha = 2\n";
        assert_eq!(err_of(text), "jobs.j: unknown key zeta");
    }
    #[test]
    fn rejects_bad_id() {
        assert_eq!(
            err_of("[jobs.Bad_Id]\ncommand = \"true\"\nevery = \"5m\"\n"),
            "jobs.Bad_Id: invalid job id \"Bad_Id\" (use a-z, 0-9 and -)"
        );
    }
    #[test]
    fn rejects_missing_command() {
        assert_eq!(err_of("[jobs.nocmd]\nevery = \"5m\"\n"), "jobs.nocmd: command is required");
    }
    #[test]
    fn rejects_bad_at_time() {
        assert_eq!(err_of(&one("at = [\"25:00\"]", "j")), "jobs.j: at: invalid time \"25:00\" (use HH:MM)");
    }
    #[test]
    fn invalid_toml_is_config_error() {
        expect_error("[jobs.x\ncommand = ", &["TOML"]);
        assert!(err_of("[jobs.x\ncommand = ").starts_with("invalid TOML: "));
    }

    #[test]
    fn manual_job_no_schedule_sorted_first() {
        let text = one("every = \"5m\"", "a-sched") + &one("manual = true", "z-tool");
        let jobs = parse_config(&text).unwrap();
        let ids: Vec<_> = jobs.iter().map(|j| j.id.as_str()).collect();
        assert_eq!(ids, ["z-tool", "a-sched"]);
        assert_eq!(jobs[0].schedule, Schedule::Manual);
    }
    #[test]
    fn manual_false_still_needs_a_schedule() {
        expect_error(&one("manual = false", "j"), &["neither"]);
    }
    #[test]
    fn rejects_manual_with_every() {
        expect_error(&one("manual = true\nevery = \"5m\"", "j"), &["manual"]);
        assert_eq!(err_of(&one("manual = true\nevery = \"5m\"", "j")), "jobs.j: a manual job has no every or at");
    }
    #[test]
    fn rejects_manual_with_quiet() {
        assert_eq!(
            err_of(&one("manual = true\nquiet = \"23:00-07:00\"", "j")),
            "jobs.j: quiet has no effect on a manual job"
        );
    }
    #[test]
    fn rejects_non_boolean_manual() {
        assert_eq!(err_of(&one("manual = \"yes\"", "j")), "jobs.j: manual must be true or false");
    }

    #[test]
    fn misc_validation_messages() {
        assert_eq!(err_of(&one("every = 5", "j")), "jobs.j: every must be a string");
        assert_eq!(err_of(&one("at = []", "j")), "jobs.j: at must be a list of \"HH:MM\" strings");
        assert_eq!(err_of(&one("at = [1]", "j")), "jobs.j: at must be a list of \"HH:MM\" strings");
        assert_eq!(err_of("[nope]\nx = 1\n"), "unknown top-level key nope");
    }

    #[test]
    fn parse_duration_units() {
        assert_eq!(parse_duration("90s").unwrap(), 90);
        assert_eq!(parse_duration("30m").unwrap(), 1800);
        assert_eq!(parse_duration("1h").unwrap(), 3600);
        assert!(parse_duration("1d").is_err());
        assert!(parse_duration("").is_err());
        assert!(parse_duration("99999999999999999999999h").is_err());
    }

    #[test]
    fn parse_quiet_window() {
        assert_eq!(parse_quiet("23:00-07:00").unwrap(), QuietWindow { start: 1380, end: 420 });
        assert_eq!(parse_quiet("09:00 - 17:30").unwrap(), QuietWindow { start: 540, end: 1050 });
        assert!(parse_quiet("09:00").is_err());
        assert!(parse_quiet("09:00-10:00-11:00").is_err());
    }

    #[test]
    fn config_accepts_stacks_table_and_ignores_it() {
        let jobs = parse_config("[stacks.a]\nbogus = 1\n\n[jobs.j]\ncommand = \"x\"\nevery = \"5m\"\n").unwrap();
        assert_eq!(jobs.iter().map(|j| j.id.as_str()).collect::<Vec<_>>(), ["j"]);
    }

    #[test]
    fn job_url_parsed_and_validated() {
        let url = |body: &str| parse_config(&format!("[jobs.j]\ncommand = \"x\"\nmanual = true\n{body}")).map(|j| j[0].url.clone());
        assert_eq!(url("url = \"http://localhost:8742/\"\n").unwrap().as_deref(), Some("http://localhost:8742/"));
        assert_eq!(url("").unwrap(), None);
        let e = url("url = \"localhost:1\"\n").unwrap_err();
        assert!(e.0.contains("url must start with"), "{}", e.0);
        assert_eq!(e.0, "jobs.j: url must start with http:// or https://");
        assert!(url("url = \"http://a b\"\n").is_err());
        assert!(url("url = \"http://\"\n").is_err());
    }

    #[test]
    fn load_config_reports_unreadable_file() {
        let e = load_config(std::path::Path::new("/nonexistent/jobs.toml")).unwrap_err();
        assert!(e.0.starts_with("cannot read /nonexistent/jobs.toml: "), "{}", e.0);
    }

    #[test]
    fn load_config_reads_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("jobs.toml");
        std::fs::write(&p, one("every = \"5m\"", "j")).unwrap();
        assert_eq!(load_config(&p).unwrap().len(), 1);
    }
}
