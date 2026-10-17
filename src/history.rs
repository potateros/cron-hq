use rusqlite::{params, Connection, Row};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Running,
    Ok,
    Failed,
    Timeout,
    Skipped,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Running => "running",
            Status::Ok => "ok",
            Status::Failed => "failed",
            Status::Timeout => "timeout",
            Status::Skipped => "skipped",
        }
    }
    pub fn parse(s: &str) -> Option<Status> {
        Some(match s {
            "running" => Status::Running,
            "ok" => Status::Ok,
            "failed" => Status::Failed,
            "timeout" => Status::Timeout,
            "skipped" => Status::Skipped,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    Scheduled,
    Manual,
}

impl Trigger {
    pub fn as_str(self) -> &'static str {
        match self {
            Trigger::Scheduled => "scheduled",
            Trigger::Manual => "manual",
        }
    }
    pub fn parse(s: &str) -> Option<Trigger> {
        match s {
            "scheduled" => Some(Trigger::Scheduled),
            "manual" => Some(Trigger::Manual),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunRow {
    pub id: i64,
    pub job: String,
    pub trigger: Trigger,
    pub started_at: i64,
    pub ended_at: Option<i64>,
    pub exit_code: Option<i32>,
    pub status: Status,
    pub reason: Option<String>,
    pub tail: Option<String>,
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS runs (
  id INTEGER PRIMARY KEY,
  job TEXT NOT NULL,
  trigger TEXT NOT NULL,
  started_at INTEGER NOT NULL,
  ended_at INTEGER,
  exit_code INTEGER,
  status TEXT NOT NULL,
  reason TEXT,
  tail TEXT
);
CREATE INDEX IF NOT EXISTS runs_job_started ON runs(job, started_at DESC);
";

const COLUMNS: &str = "id, job, trigger, started_at, ended_at, exit_code, status, reason, tail";

fn conversion_error(col: usize, what: &str, v: String) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        col,
        rusqlite::types::Type::Text,
        format!("unknown {what} {v:?}").into(),
    )
}

fn row_to_run(r: &Row) -> rusqlite::Result<RunRow> {
    let trigger: String = r.get(2)?;
    let status: String = r.get(6)?;
    Ok(RunRow {
        id: r.get(0)?,
        job: r.get(1)?,
        trigger: Trigger::parse(&trigger)
            .ok_or_else(|| conversion_error(2, "trigger", trigger.clone()))?,
        started_at: r.get(3)?,
        ended_at: r.get(4)?,
        exit_code: r.get(5)?,
        status: Status::parse(&status)
            .ok_or_else(|| conversion_error(6, "status", status.clone()))?,
        reason: r.get(7)?,
        tail: r.get(8)?,
    })
}

pub struct History {
    db: Connection,
}

impl History {
    pub fn open(path: &Path) -> rusqlite::Result<History> {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let db = Connection::open(path)?;
        db.execute_batch("PRAGMA busy_timeout = 5000; PRAGMA journal_mode = WAL;")?;
        db.execute_batch(SCHEMA)?;
        Ok(History { db })
    }

    pub fn start(&self, job: &str, trigger: Trigger, started_at: i64) -> i64 {
        self.db
            .query_row(
                "INSERT INTO runs (job, trigger, started_at, status) VALUES (?, ?, ?, 'running') RETURNING id",
                params![job, trigger.as_str(), started_at],
                |r| r.get(0),
            )
            .expect("insert run")
    }

    pub fn finish(
        &self,
        id: i64,
        ended_at: i64,
        exit_code: Option<i32>,
        status: Status,
        tail: &str,
    ) {
        self.db
            .execute(
                "UPDATE runs SET ended_at = ?, exit_code = ?, status = ?, tail = ? WHERE id = ?",
                params![ended_at, exit_code, status.as_str(), tail, id],
            )
            .expect("finish run");
    }

    /// Close runs still marked running; call only while holding the job's lock, so none can be live.
    pub fn fail_stale(&self, job: &str, at: i64, reason: &str) -> usize {
        self.db
            .execute(
                "UPDATE runs SET status = 'failed', ended_at = ?, tail = COALESCE(tail || char(10), '') || ? WHERE job = ? AND status = 'running'",
                params![at, reason, job],
            )
            .expect("fail stale runs")
    }

    pub fn skip(&self, job: &str, trigger: Trigger, reason: &str, at: i64) -> i64 {
        self.db
            .query_row(
                "INSERT INTO runs (job, trigger, started_at, ended_at, status, reason) VALUES (?, ?, ?, ?, 'skipped', ?) RETURNING id",
                params![job, trigger.as_str(), at, at, reason],
                |r| r.get(0),
            )
            .expect("insert skip")
    }

    pub fn recent(&self, job: &str, limit: usize) -> Vec<RunRow> {
        let sql = format!(
            "SELECT {COLUMNS} FROM runs WHERE job = ? ORDER BY started_at DESC, id DESC LIMIT ?"
        );
        let mut stmt = self.db.prepare_cached(&sql).expect("prepare recent");
        let rows = stmt
            .query_map(params![job, limit as i64], row_to_run)
            .expect("query recent");
        rows.filter_map(Result::ok).collect()
    }

    pub fn last_real(&self, job: &str) -> Option<RunRow> {
        let sql = format!(
            "SELECT {COLUMNS} FROM runs WHERE job = ? AND status != 'skipped' ORDER BY started_at DESC, id DESC LIMIT 1"
        );
        self.db.query_row(&sql, params![job], row_to_run).ok()
    }

    pub fn prune(&self, now: i64, max_age_ms: i64) -> usize {
        self.db
            .execute(
                "DELETE FROM runs WHERE started_at < ?",
                params![now - max_age_ms],
            )
            .expect("prune runs")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open() -> (tempfile::TempDir, History) {
        let dir = tempfile::tempdir().unwrap();
        let h = History::open(&dir.path().join("sub/history.db")).unwrap();
        (dir, h)
    }

    #[test]
    fn start_finish_round_trip() {
        let (_d, h) = open();
        let id = h.start("a", Trigger::Manual, 1000);
        assert_eq!(h.recent("a", 5)[0].status, Status::Running);
        h.finish(id, 2500, Some(0), Status::Ok, "hi");
        assert_eq!(
            h.recent("a", 5)[0],
            RunRow {
                id,
                job: "a".into(),
                trigger: Trigger::Manual,
                started_at: 1000,
                ended_at: Some(2500),
                exit_code: Some(0),
                status: Status::Ok,
                reason: None,
                tail: Some("hi".into()),
            }
        );
    }

    #[test]
    fn skip_records_reason() {
        let (_d, h) = open();
        h.skip("a", Trigger::Scheduled, "quiet", 5000);
        let r = &h.recent("a", 1)[0];
        assert_eq!(
            (r.status, r.reason.as_deref(), r.started_at, r.ended_at),
            (Status::Skipped, Some("quiet"), 5000, Some(5000))
        );
    }

    #[test]
    fn recent_orders_newest_first_and_limits() {
        let (_d, h) = open();
        for t in [1, 3, 2, 4] {
            h.skip("a", Trigger::Scheduled, "quiet", t);
        }
        h.skip("b", Trigger::Scheduled, "quiet", 9);
        let got: Vec<i64> = h.recent("a", 3).iter().map(|r| r.started_at).collect();
        assert_eq!(got, [4, 3, 2]);
    }

    #[test]
    fn last_real_ignores_skipped() {
        let (_d, h) = open();
        let id = h.start("a", Trigger::Scheduled, 10);
        h.finish(id, 11, Some(1), Status::Failed, "");
        h.skip("a", Trigger::Scheduled, "overlap", 20);
        assert_eq!(h.last_real("a").unwrap().status, Status::Failed);
        assert!(h.last_real("nope").is_none());
    }

    #[test]
    fn prune_deletes_only_rows_older_than_90_days() {
        let (_d, h) = open();
        let day = 86_400_000;
        let now = 200 * day;
        h.skip("a", Trigger::Scheduled, "quiet", now - 91 * day);
        h.skip("a", Trigger::Scheduled, "quiet", now - 89 * day);
        assert_eq!(h.prune(now, 90 * day), 1);
        assert_eq!(h.recent("a", 10).len(), 1);
    }

    #[test]
    fn concurrent_reader_and_writer_do_not_hit_busy() {
        let (dir, h) = open();
        let reader = History::open(&dir.path().join("sub/history.db")).unwrap();
        for i in 0..200 {
            let id = h.start("c", Trigger::Scheduled, i);
            reader.recent("c", 10);
            h.finish(id, i + 1, Some(0), Status::Ok, "");
            reader.last_real("c");
        }
        assert_eq!(reader.recent("c", 500).len(), 200);
    }

    #[test]
    fn fail_stale_closes_running_and_appends_reason() {
        let (_d, h) = open();
        let a = h.start("a", Trigger::Scheduled, 1);
        let b = h.start("a", Trigger::Scheduled, 2);
        h.finish(b, 3, Some(0), Status::Ok, "done");
        let c = h.start("a", Trigger::Scheduled, 4);
        h.finish(c, 5, None, Status::Failed, "");
        let d = h.start("a", Trigger::Scheduled, 6);
        let _ = d;
        assert_eq!(h.fail_stale("a", 99, "stale"), 2);
        let rows = h.recent("a", 10);
        let by = |id: i64| rows.iter().find(|r| r.id == id).unwrap().clone();
        assert_eq!(
            (by(a).status, by(a).ended_at, by(a).tail.as_deref()),
            (Status::Failed, Some(99), Some("stale"))
        );
        assert_eq!(by(b).tail.as_deref(), Some("done"));
        assert_eq!(by(d).tail.as_deref(), Some("stale"));
        // a finished row with a tail would get a newline-joined reason
        let e = h.start("z", Trigger::Scheduled, 1);
        h.finish(e, 2, None, Status::Failed, "");
        assert_eq!(h.fail_stale("z", 3, "x"), 0);
    }

    #[test]
    fn fail_stale_joins_existing_tail_with_newline() {
        let (_d, h) = open();
        let id = h.start("a", Trigger::Scheduled, 1);
        h.db.execute("UPDATE runs SET tail = 'partial' WHERE id = ?", [id])
            .unwrap();
        h.fail_stale("a", 9, "stale");
        assert_eq!(h.recent("a", 1)[0].tail.as_deref(), Some("partial\nstale"));
    }

    #[test]
    fn status_and_trigger_strings() {
        for (s, t) in [
            (Status::Running, "running"),
            (Status::Ok, "ok"),
            (Status::Failed, "failed"),
            (Status::Timeout, "timeout"),
            (Status::Skipped, "skipped"),
        ] {
            assert_eq!(s.as_str(), t);
            assert_eq!(Status::parse(t), Some(s));
        }
        assert_eq!(Status::parse("bogus"), None);
        assert_eq!(Trigger::Scheduled.as_str(), "scheduled");
        assert_eq!(Trigger::parse("manual"), Some(Trigger::Manual));
        assert_eq!(Trigger::parse("x"), None);
    }

    #[test]
    fn opens_db_created_by_ts_schema() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.db");
        {
            let c = Connection::open(&path).unwrap();
            c.execute_batch(
                "
CREATE TABLE IF NOT EXISTS runs (
  id INTEGER PRIMARY KEY,
  job TEXT NOT NULL,
  trigger TEXT NOT NULL,
  started_at INTEGER NOT NULL,
  ended_at INTEGER,
  exit_code INTEGER,
  status TEXT NOT NULL,
  reason TEXT,
  tail TEXT
);
CREATE INDEX IF NOT EXISTS runs_job_started ON runs(job, started_at DESC);
INSERT INTO runs (job, trigger, started_at, ended_at, exit_code, status, tail) VALUES ('a', 'scheduled', 10, 12, 0, 'ok', 'out');
INSERT INTO runs (job, trigger, started_at, ended_at, status, reason, tail) VALUES ('a', 'scheduled', 20, 20, 'skipped', 'quiet', NULL);
",
            )
            .unwrap();
        }
        let h = History::open(&path).unwrap();
        let rows = h.recent("a", 10);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].status, Status::Skipped);
        assert_eq!(rows[0].tail, None);
        assert_eq!(rows[0].exit_code, None);
        let real = h.last_real("a").unwrap();
        assert_eq!(
            (real.status, real.tail.as_deref(), real.exit_code),
            (Status::Ok, Some("out"), Some(0))
        );
    }
}
