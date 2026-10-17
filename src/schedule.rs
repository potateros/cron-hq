use crate::config::{Job, QuietWindow, Schedule};
use chrono::{DateTime, Duration, Local, NaiveDate, NaiveTime, TimeZone, Timelike};

fn minutes_of(t: DateTime<Local>) -> u32 {
    t.hour() * 60 + t.minute()
}

pub fn in_quiet(q: Option<&QuietWindow>, at: DateTime<Local>) -> bool {
    let Some(q) = q else { return false };
    let m = minutes_of(at) as u16;
    if q.start <= q.end {
        m >= q.start && m < q.end
    } else {
        m >= q.start || m < q.end
    }
}

/// Local time `hour:minute` on `date`; a time skipped by DST moves forward like JS `setHours`.
fn local_at(date: NaiveDate, hour: u32, minute: u32) -> Option<DateTime<Local>> {
    let mut naive = date.and_time(NaiveTime::from_hms_opt(hour, minute, 0)?);
    for _ in 0..4 {
        if let Some(t) = Local.from_local_datetime(&naive).earliest() {
            return Some(t);
        }
        naive += Duration::hours(1);
    }
    None
}

fn quiet_end_after(q: &QuietWindow, t: DateTime<Local>) -> DateTime<Local> {
    let (h, m) = (u32::from(q.end / 60), u32::from(q.end % 60));
    let end = local_at(t.date_naive(), h, m).unwrap_or(t);
    if end <= t {
        local_at(t.date_naive() + Duration::days(1), h, m).unwrap_or(t)
    } else {
        end
    }
}

pub fn next_run(job: &Job, last_start: Option<DateTime<Local>>, now: DateTime<Local>) -> Option<DateTime<Local>> {
    match &job.schedule {
        Schedule::Manual => None,
        Schedule::Every { seconds } => {
            let step = Duration::seconds(*seconds as i64);
            let next = match last_start.map(|l| l + step) {
                Some(n) if n > now => n,
                _ => now + step,
            };
            Some(match &job.quiet {
                Some(q) if in_quiet(Some(q), next) => quiet_end_after(q, next),
                _ => next,
            })
        }
        Schedule::At { times } => {
            let mut times = times.clone();
            times.sort_by_key(|&(h, m)| u32::from(h) * 60 + u32::from(m));
            for day in 0..8 {
                let date = now.date_naive() + Duration::days(day);
                for &(h, m) in &times {
                    if let Some(c) = local_at(date, u32::from(h), u32::from(m)) {
                        if c > now && !in_quiet(job.quiet.as_ref(), c) {
                            return Some(c);
                        }
                    }
                }
            }
            // Every time is inside quiet hours: the runner will skip them all; report the first anyway.
            let &(h, m) = times.first()?;
            local_at(now.date_naive() + Duration::days(1), u32::from(h), u32::from(m))
        }
    }
}

pub fn format_countdown(ms: i64) -> String {
    let total = (ms / 1000).max(0);
    format!("T–{:02}:{:02}:{:02}", total / 3600, (total % 3600) / 60, total % 60)
}

pub fn describe_schedule(s: &Schedule) -> String {
    match s {
        Schedule::Manual => "on demand".to_string(),
        Schedule::At { times } => {
            let t: Vec<_> = times.iter().map(|(h, m)| format!("{h:02}:{m:02}")).collect();
            format!("daily {}", t.join(", "))
        }
        Schedule::Every { seconds } if seconds % 3600 == 0 => format!("every {}h", seconds / 3600),
        Schedule::Every { seconds } if seconds % 60 == 0 => format!("every {}m", seconds / 60),
        Schedule::Every { seconds } => format!("every {seconds}s"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Job, QuietWindow, Schedule};
    use chrono::{DateTime, Local, TimeZone};
    use std::path::PathBuf;

    fn d(day: u32, h: u32, m: u32) -> DateTime<Local> {
        Local.with_ymd_and_hms(2026, 10, day, h, m, 0).unwrap() // Oct 2026, local time
    }
    const NIGHT: QuietWindow = QuietWindow { start: 23 * 60, end: 7 * 60 };

    fn job(schedule: Schedule, quiet: Option<QuietWindow>) -> Job {
        Job {
            id: "j".into(),
            description: String::new(),
            command: "true".into(),
            cwd: PathBuf::from("/"),
            schedule,
            quiet,
            timeout_sec: 600,
            url: None,
        }
    }
    fn every() -> Job {
        job(Schedule::Every { seconds: 1800 }, None)
    }

    #[test]
    fn manual_never_has_a_next_run() {
        let m = job(Schedule::Manual, None);
        assert_eq!(next_run(&m, None, d(16, 17, 0)), None);
    }
    #[test]
    fn manual_described_as_on_demand() {
        assert_eq!(describe_schedule(&Schedule::Manual), "on demand");
    }

    #[test]
    fn in_quiet_wrapping_window() {
        for (h, m) in [(23, 0), (2, 0), (6, 59)] {
            assert!(in_quiet(Some(&NIGHT), d(16, h, m)), "{h}:{m}");
        }
        for (h, m) in [(7, 0), (12, 0), (22, 59)] {
            assert!(!in_quiet(Some(&NIGHT), d(16, h, m)), "{h}:{m}");
        }
    }
    #[test]
    fn in_quiet_plain_window() {
        let day = QuietWindow { start: 540, end: 1020 };
        assert!(in_quiet(Some(&day), d(16, 9, 0)));
        assert!(!in_quiet(Some(&day), d(16, 17, 0)));
    }
    #[test]
    fn in_quiet_null_window_is_never_quiet() {
        assert!(!in_quiet(None, d(16, 2, 0)));
    }

    #[test]
    fn every_last_start_plus_interval() {
        assert_eq!(next_run(&every(), Some(d(16, 17, 30)), d(16, 17, 32)), Some(d(16, 18, 0)));
    }
    #[test]
    fn every_never_run_is_now_plus_interval() {
        assert_eq!(next_run(&every(), None, d(16, 17, 32)), Some(d(16, 18, 2)));
    }
    #[test]
    fn every_missed_is_now_plus_interval() {
        assert_eq!(next_run(&every(), Some(d(16, 15, 0)), d(16, 17, 32)), Some(d(16, 18, 2)));
    }
    #[test]
    fn every_lands_in_quiet_goes_to_quiet_end() {
        let j = job(Schedule::Every { seconds: 1800 }, Some(NIGHT));
        assert_eq!(next_run(&j, Some(d(16, 22, 40)), d(16, 22, 45)), Some(d(17, 7, 0)));
    }

    #[test]
    fn at_single_time_later_is_tomorrow() {
        let j = job(Schedule::At { times: vec![(6, 55)] }, None);
        assert_eq!(next_run(&j, None, d(16, 17, 32)), Some(d(17, 6, 55)));
    }
    #[test]
    fn at_picks_the_next_of_several() {
        let j = job(Schedule::At { times: vec![(6, 55), (19, 30)] }, None);
        assert_eq!(next_run(&j, None, d(16, 17, 32)), Some(d(16, 19, 30)));
    }
    #[test]
    fn at_skips_times_inside_quiet() {
        let j = job(Schedule::At { times: vec![(23, 30), (8, 0)] }, Some(NIGHT));
        assert_eq!(next_run(&j, None, d(16, 17, 32)), Some(d(17, 8, 0)));
    }
    #[test]
    fn at_all_inside_quiet_reports_first_tomorrow() {
        let j = job(Schedule::At { times: vec![(23, 30), (2, 0)] }, Some(NIGHT));
        assert_eq!(next_run(&j, None, d(16, 17, 32)), Some(d(17, 2, 0)));
    }

    #[test]
    fn countdown_hh_mm_ss() {
        assert_eq!(format_countdown(1672000), "T–00:27:52");
    }
    #[test]
    fn countdown_negative_clamps() {
        assert_eq!(format_countdown(-5), "T–00:00:00");
    }
    #[test]
    fn countdown_over_a_day() {
        assert_eq!(format_countdown(90061000), "T–25:01:01");
    }

    #[test]
    fn describe_every() {
        assert_eq!(describe_schedule(&Schedule::Every { seconds: 1800 }), "every 30m");
        assert_eq!(describe_schedule(&Schedule::Every { seconds: 3600 }), "every 1h");
        assert_eq!(describe_schedule(&Schedule::Every { seconds: 90 }), "every 90s");
    }
    #[test]
    fn describe_at() {
        assert_eq!(describe_schedule(&Schedule::At { times: vec![(6, 55), (19, 30)] }), "daily 06:55, 19:30");
    }
}
