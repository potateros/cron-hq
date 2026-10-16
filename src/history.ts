import { Database } from "bun:sqlite";
import { mkdirSync } from "node:fs";
import { dirname } from "node:path";
import { dbPath } from "./paths";

export type Status = "running" | "ok" | "failed" | "timeout" | "skipped";
export type Trigger = "scheduled" | "manual";
export type SkipReason = "paused" | "quiet" | "overlap";

export interface RunRow {
  id: number;
  job: string;
  trigger: Trigger;
  startedAt: number;
  endedAt: number | null;
  exitCode: number | null;
  status: Status;
  reason: string | null;
  tail: string | null;
}

const SCHEMA = `
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
`;

const COLUMNS = `id, job, trigger, started_at AS startedAt, ended_at AS endedAt, exit_code AS exitCode,
  status, reason, tail`;

export class History {
  private db: Database;

  constructor(path = dbPath()) {
    mkdirSync(dirname(path), { recursive: true });
    this.db = new Database(path, { create: true });
    this.db.exec("PRAGMA busy_timeout = 5000");
    this.db.exec("PRAGMA journal_mode = WAL");
    this.db.exec(SCHEMA);
  }

  start(job: string, trigger: Trigger, startedAt: number): number {
    const r = this.db
      .query("INSERT INTO runs (job, trigger, started_at, status) VALUES (?, ?, ?, 'running') RETURNING id")
      .get(job, trigger, startedAt) as { id: number };
    return r.id;
  }

  finish(
    id: number,
    r: { endedAt: number; exitCode: number | null; status: Exclude<Status, "running" | "skipped">; tail: string },
  ): void {
    this.db
      .query("UPDATE runs SET ended_at = ?, exit_code = ?, status = ?, tail = ? WHERE id = ?")
      .run(r.endedAt, r.exitCode, r.status, r.tail, id);
  }

  /** Close runs still marked running; call only while holding the job's lock, so none can be live. */
  failStale(job: string, at: number, reason: string): number {
    return this.db
      .query("UPDATE runs SET status = 'failed', ended_at = ?, tail = COALESCE(tail || char(10), '') || ? WHERE job = ? AND status = 'running'")
      .run(at, reason, job).changes;
  }

  skip(job: string, trigger: Trigger, reason: SkipReason, at: number): number {
    const r = this.db
      .query(
        "INSERT INTO runs (job, trigger, started_at, ended_at, status, reason) VALUES (?, ?, ?, ?, 'skipped', ?) RETURNING id",
      )
      .get(job, trigger, at, at, reason) as { id: number };
    return r.id;
  }

  recent(job: string, limit: number): RunRow[] {
    return this.db
      .query(`SELECT ${COLUMNS} FROM runs WHERE job = ? ORDER BY started_at DESC, id DESC LIMIT ?`)
      .all(job, limit) as RunRow[];
  }

  lastReal(job: string): RunRow | null {
    return (this.db
      .query(`SELECT ${COLUMNS} FROM runs WHERE job = ? AND status != 'skipped' ORDER BY started_at DESC, id DESC LIMIT 1`)
      .get(job) ?? null) as RunRow | null;
  }

  prune(now: number, maxAgeMs = 90 * 86400_000): number {
    return this.db.query("DELETE FROM runs WHERE started_at < ?").run(now - maxAgeMs).changes;
  }

  close(): void {
    this.db.close();
  }
}
