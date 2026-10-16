import { spawn } from "node:child_process";
import { closeSync, existsSync, mkdirSync, openSync, readFileSync, statSync, unlinkSync, writeSync } from "node:fs";
import { homedir } from "node:os";
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

/**
 * launchd starts us with PATH=/usr/bin:/bin, and `zsh -lc` only adds what ~/.zprofile sets (Homebrew).
 * User tool dirs are usually added in ~/.zshrc, which non-interactive shells skip, so add the common ones.
 */
export function jobEnv(): NodeJS.ProcessEnv {
  const home = homedir();
  const extra = [`${home}/.local/bin`, `${home}/.bun/bin`, "/opt/homebrew/bin", "/usr/local/bin"];
  return { ...process.env, PATH: [...extra, process.env.PATH || "/usr/bin:/bin"].join(":") };
}

export interface Execution {
  done: Promise<{ code: number | null; timedOut: boolean }>;
  /** Forward a signal to the job's process group, escalating to SIGKILL after the grace period. */
  stop: (sig: NodeJS.Signals) => void;
}

/** How long to wait for stdout/stderr to close after the shell exits (a stray grandchild may hold them). */
const DRAIN_MS = 2000;

/** Run the command in its own process group; resolves with exit code (null if killed by timeout). */
function execute(job: Job, onOutput: (chunk: string) => void, killGraceMs: number): Execution {
  const child = spawn("/bin/zsh", ["-lc", job.command], {
    cwd: job.cwd,
    env: jobEnv(),
    detached: true,
    stdio: ["ignore", "pipe", "pipe"],
  });
  const signal = (sig: NodeJS.Signals) => {
    try {
      process.kill(-child.pid!, sig);
    } catch {}
  };
  let killTimer: Timer | undefined;
  const stop = (sig: NodeJS.Signals) => {
    signal(sig);
    killTimer ??= setTimeout(() => signal("SIGKILL"), killGraceMs);
  };
  const done = new Promise<{ code: number | null; timedOut: boolean }>((resolve) => {
    let timedOut = false;
    let drainTimer: Timer | undefined;
    const timer = setTimeout(() => {
      timedOut = true;
      stop("SIGTERM");
    }, job.timeoutSec * 1000);
    const finish = (code: number | null) => {
      clearTimeout(timer);
      clearTimeout(killTimer);
      clearTimeout(drainTimer);
      if (timedOut) signal("SIGKILL"); // make sure no stragglers survive in the group
      resolve({ code: timedOut ? null : code ?? 1, timedOut });
    };

    child.stdout!.setEncoding("utf8").on("data", onOutput);
    child.stderr!.setEncoding("utf8").on("data", onOutput);
    child.on("error", (e) => {
      onOutput(`hq: failed to start: ${e.message}\n`);
      finish(1);
    });
    child.on("exit", (code) => {
      // close waits for every holder of the pipes; don't let an escaped grandchild hang us.
      drainTimer = setTimeout(() => {
        child.stdout!.destroy();
        child.stderr!.destroy();
        finish(code);
      }, DRAIN_MS);
    });
    child.on("close", (code) => finish(code));
  });
  return { done, stop };
}

const FORWARDED: NodeJS.Signals[] = ["SIGTERM", "SIGINT", "SIGHUP"];

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
    history.failStale(job.id, started.getTime(), "hq: runner died before the run finished");
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
    let killedBy: NodeJS.Signals | null = null;
    const cwdOk = existsSync(job.cwd) && statSync(job.cwd).isDirectory();
    if (!cwdOk) {
      output(`hq: working directory does not exist or is not a directory: ${job.cwd}\n`);
      code = 1;
      status = "failed";
    } else {
      let r: { code: number | null; timedOut: boolean };
      try {
        const run = execute(job, output, opts.killGraceMs ?? 10_000);
        // launchd sends SIGTERM on bootout (hold, sync): stop the job and still record the run.
        const onSignal = (sig: NodeJS.Signals) => {
          killedBy ??= sig;
          run.stop("SIGTERM");
        };
        for (const sig of FORWARDED) process.on(sig, onSignal);
        try {
          r = await run.done;
        } finally {
          for (const sig of FORWARDED) process.off(sig, onSignal);
        }
      } catch (e) {
        output(`hq: failed to start: ${(e as Error).message}\n`);
        r = { code: 1, timedOut: false };
      }
      if (killedBy) output(`hq: killed by ${killedBy}\n`);
      code = r.code;
      status = r.timedOut ? "timeout" : code === 0 && !killedBy ? "ok" : "failed";
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
