import { spawn } from "node:child_process";
import { closeSync, existsSync, mkdirSync, openSync, readFileSync, unlinkSync, writeSync } from "node:fs";
import { dirname } from "node:path";
import type { Job } from "./config";
import type { History, Status, Trigger } from "./history";
import { appendLog, rotateIfNeeded, TailBuffer } from "./log";
import { lockPath } from "./paths";
import { inQuiet } from "./schedule";
import { readPaused } from "./state";

export const TIMEOUT_EXIT = 124;

interface RunOpts {
  manual: boolean;
  history: History;
  now?: () => Date;
  killGraceMs?: number;
}

const pad = (n: number, w = 2) => String(n).padStart(w, "0");
const stamp = (t: Date) =>
  `${t.getFullYear()}-${pad(t.getMonth() + 1)}-${pad(t.getDate())} ${pad(t.getHours())}:${pad(t.getMinutes())}:${pad(t.getSeconds())}`;

export function formatDuration(ms: number): string {
  const s = Math.round(ms / 1000);
  return s < 60 ? `${s}s` : `${Math.floor(s / 60)}m${pad(s % 60)}s`;
}

function isAlive(pid: number): boolean {
  try {
    process.kill(pid, 0);
    return true;
  } catch (e) {
    return (e as NodeJS.ErrnoException).code === "EPERM";
  }
}

/** Returns true if we now hold the lock. */
function acquireLock(id: string): boolean {
  const path = lockPath(id);
  mkdirSync(dirname(path), { recursive: true });
  for (let attempt = 0; attempt < 2; attempt++) {
    try {
      const fd = openSync(path, "wx");
      writeSync(fd, String(process.pid));
      closeSync(fd);
      return true;
    } catch (e) {
      if ((e as NodeJS.ErrnoException).code !== "EEXIST") throw e;
      const pid = Number(readFileSync(path, "utf8").trim());
      if (pid && isAlive(pid)) return false;
      unlinkSync(path); // stale: holder is gone
    }
  }
  return false;
}

function releaseLock(id: string): void {
  try {
    unlinkSync(lockPath(id));
  } catch {}
}

/** Run the command in its own process group; resolves with exit code (null if killed by timeout). */
function execute(job: Job, onOutput: (chunk: string) => void, killGraceMs: number) {
  return new Promise<{ code: number | null; timedOut: boolean }>((resolve) => {
    const child = spawn("/bin/zsh", ["-lc", job.command], {
      cwd: job.cwd,
      detached: true,
      stdio: ["ignore", "pipe", "pipe"],
    });
    const signal = (sig: NodeJS.Signals) => {
      try {
        process.kill(-child.pid!, sig);
      } catch {}
    };
    let timedOut = false;
    let killTimer: Timer | undefined;
    const timer = setTimeout(() => {
      timedOut = true;
      signal("SIGTERM");
      killTimer = setTimeout(() => signal("SIGKILL"), killGraceMs);
    }, job.timeoutSec * 1000);

    child.stdout!.setEncoding("utf8").on("data", onOutput);
    child.stderr!.setEncoding("utf8").on("data", onOutput);
    child.on("error", (e) => onOutput(`hq: failed to start: ${e.message}\n`));
    child.on("close", (code) => {
      clearTimeout(timer);
      clearTimeout(killTimer);
      if (timedOut) signal("SIGKILL"); // make sure no stragglers survive in the group
      resolve({ code: timedOut ? null : code ?? 1, timedOut });
    });
  });
}

export async function runJob(job: Job, opts: RunOpts): Promise<number> {
  const now = opts.now ?? (() => new Date());
  const trigger: Trigger = opts.manual ? "manual" : "scheduled";
  const { history } = opts;

  if (!opts.manual && readPaused().has(job.id)) {
    history.skip(job.id, trigger, "paused", now().getTime());
    return 0;
  }
  if (!opts.manual && inQuiet(job.quiet, now())) {
    history.skip(job.id, trigger, "quiet", now().getTime());
    return 0;
  }
  if (!acquireLock(job.id)) {
    history.skip(job.id, trigger, "overlap", now().getTime());
    return 0;
  }

  try {
    const started = now();
    const runId = history.start(job.id, trigger, started.getTime());
    const num = pad(runId, 4);
    rotateIfNeeded(job.id); // before the header, so the current log always holds the latest run
    appendLog(job.id, `── RUN ${num} · ${stamp(started)} · ${trigger} ──\n`);

    const tail = new TailBuffer(20);
    const output = (chunk: string) => {
      appendLog(job.id, chunk);
      tail.push(chunk);
    };

    let code: number | null;
    let status: Exclude<Status, "running" | "skipped">;
    if (!existsSync(job.cwd)) {
      output(`hq: working directory does not exist: ${job.cwd}\n`);
      code = 1;
      status = "failed";
    } else {
      const r = await execute(job, output, opts.killGraceMs ?? 10_000);
      code = r.code;
      status = r.timedOut ? "timeout" : code === 0 ? "ok" : "failed";
    }

    const ended = now();
    const label = status === "ok" ? "NOMINAL" : status === "timeout" ? "TIMEOUT" : "FAULT";
    const exitText = code === null ? "killed" : `exit ${code}`;
    appendLog(job.id, `── END ${num} · ${label} · ${formatDuration(ended.getTime() - started.getTime())} · ${exitText} ──\n`);
    history.finish(runId, { endedAt: ended.getTime(), exitCode: code, status, tail: tail.text() });
    history.prune(ended.getTime());
    return code ?? TIMEOUT_EXIT;
  } finally {
    releaseLock(job.id);
  }
}
