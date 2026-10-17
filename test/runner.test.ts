import { beforeEach, describe, expect, test } from "bun:test";
import { existsSync, mkdirSync, readFileSync, statSync, writeFileSync } from "node:fs";
import { homedir } from "node:os";
import { dirname, join } from "node:path";
import type { Job } from "../src/config";
import { History } from "../src/history";
import { lockPath, logPath } from "../src/paths";
import { runJob } from "../src/runner";
import { setPaused } from "../src/state";
import { tempHome } from "./helpers";

let dir: string;
let history: History;
beforeEach(() => {
  dir = tempHome();
  history = new History();
});

const job = (command: string, over: Partial<Job> = {}): Job => ({
  id: "t", description: "", command, cwd: dir, quiet: null, timeoutSec: 60, url: null,
  schedule: { kind: "every", seconds: 300 }, ...over,
});
const last = () => history.recent("t", 1)[0]!;
const log = () => readFileSync(logPath("t"), "utf8");

describe("runJob", () => {
  test("ok run", async () => {
    expect(await runJob(job("echo hi"), { manual: false, history })).toBe(0);
    expect([last().status, last().exitCode, last().tail, last().trigger]).toEqual(["ok", 0, "hi", "scheduled"]);
    const id = String(last().id).padStart(4, "0");
    expect(log()).toMatch(new RegExp(`── RUN ${id} · \\d{4}-\\d\\d-\\d\\d \\d\\d:\\d\\d:\\d\\d · scheduled ──`));
    expect(log()).toContain(`── END ${id} · NOMINAL · `);
    expect(log()).toContain("· exit 0 ──");
  });

  test("failed run", async () => {
    expect(await runJob(job("exit 3"), { manual: true, history })).toBe(3);
    expect([last().status, last().exitCode, last().trigger]).toEqual(["failed", 3, "manual"]);
    expect(log()).toMatch(/── END \d{4} · FAULT · .* · exit 3 ──/);
  });

  test("timeout kills the job", async () => {
    const t0 = Date.now();
    expect(await runJob(job("sleep 30", { timeoutSec: 1 }), { manual: false, history, killGraceMs: 200 })).toBe(124);
    expect(last().status).toBe("timeout");
    expect(Date.now() - t0).toBeLessThan(3000);
    expect(log()).toContain("· TIMEOUT ·");
  });

  test("timeout escalates to SIGKILL when TERM is ignored", async () => {
    const t0 = Date.now();
    await runJob(job("trap '' TERM; sleep 30; echo late", { timeoutSec: 1 }), { manual: false, history, killGraceMs: 200 });
    expect(last().status).toBe("timeout");
    expect(Date.now() - t0).toBeLessThan(3000);
  });

  test("held job is skipped, but manual runs anyway", async () => {
    const marker = join(dir, "ran");
    setPaused("t", true);
    expect(await runJob(job(`touch ${marker}`), { manual: false, history })).toBe(0);
    expect([last().status, last().reason]).toEqual(["skipped", "paused"]);
    expect(existsSync(marker)).toBe(false);
    await runJob(job(`touch ${marker}`), { manual: true, history });
    expect(existsSync(marker)).toBe(true);
  });

  test("quiet hours skip", async () => {
    const quiet = { start: 23 * 60, end: 7 * 60 };
    await runJob(job("true", { quiet }), { manual: false, history, now: () => new Date(2026, 9, 17, 2, 0) });
    expect([last().status, last().reason]).toEqual(["skipped", "quiet"]);
  });

  test("overlap skip when a live process holds the lock", async () => {
    mkdirSync(dirname(lockPath("t")), { recursive: true });
    writeFileSync(lockPath("t"), String(process.pid));
    await runJob(job("true"), { manual: false, history });
    expect([last().status, last().reason]).toEqual(["skipped", "overlap"]);
  });

  test("stale lock is taken over", async () => {
    mkdirSync(dirname(lockPath("t")), { recursive: true });
    writeFileSync(lockPath("t"), "999999");
    await runJob(job("true"), { manual: false, history });
    expect(last().status).toBe("ok");
    expect(existsSync(lockPath("t"))).toBe(false);
  });

  test("missing cwd is a recorded failure, not a crash", async () => {
    expect(await runJob(job("true", { cwd: "/nonexistent/dir" }), { manual: false, history })).toBe(1);
    expect(last().status).toBe("failed");
    expect(last().tail).toContain("/nonexistent/dir");
  });

  test("missing command is a recorded failure", async () => {
    expect(await runJob(job("definitely-not-a-cmd"), { manual: false, history })).toBe(127);
    expect(last().status).toBe("failed");
  });

  test("tail keeps the last 20 lines including a partial last line", async () => {
    await runJob(job("seq 1 50; printf tail-no-newline"), { manual: false, history });
    const lines = last().tail!.split("\n");
    expect(lines).toHaveLength(20);
    expect(lines.at(-1)).toBe("tail-no-newline");
    expect(lines[0]).toBe("32");
    expect(log()).toContain("1\n2\n3\n");
    expect(log()).toContain("50\ntail-no-newline");
  });

  test("user tool dirs are on PATH (they live in .zshrc, which zsh -lc skips)", async () => {
    const saved = process.env.PATH;
    process.env.PATH = "/usr/bin:/bin"; // what launchd gives us
    try {
      await runJob(job("echo $PATH"), { manual: false, history });
    } finally {
      process.env.PATH = saved;
    }
    const path = last().tail!;
    expect(path).toContain(`${homedir()}/.local/bin`);
    expect(path).toContain(`${homedir()}/.bun/bin`);
  });

  test("cwd that is a file is a recorded failure, not a throw", async () => {
    const file = join(dir, "not-a-dir");
    writeFileSync(file, "");
    expect(await runJob(job("echo hi", { cwd: file }), { manual: false, history })).toBe(1);
    expect(last().status).toBe("failed");
    expect(last().tail).toContain("not a directory");
    expect(log()).toMatch(/── END \d{4} · FAULT · /);
    expect(existsSync(lockPath("t"))).toBe(false);
  });

  test("a run left 'running' by a dead runner is closed by the next run", async () => {
    const stale = history.start("t", "scheduled", Date.now() - 60_000);
    await runJob(job("true"), { manual: false, history });
    const row = history.recent("t", 10).find((r) => r.id === stale)!;
    expect(row.status).toBe("failed");
    expect(row.tail).toContain("runner died");
    expect(row.endedAt).not.toBeNull();
  });

  test("a grandchild that escapes the group and holds the pipe does not hang the run", async () => {
    const t0 = Date.now();
    await runJob(job(`perl -MPOSIX -e 'fork and exit; POSIX::setsid(); sleep 20'; echo done`), { manual: false, history });
    expect(Date.now() - t0).toBeLessThan(6000);
    expect([last().status, last().tail]).toEqual(["ok", "done"]);
  }, 30_000);

  test("log rotates past 1 MB", async () => {
    mkdirSync(dirname(logPath("t")), { recursive: true });
    writeFileSync(logPath("t"), "x".repeat(1_150_000));
    await runJob(job("echo hi"), { manual: false, history });
    expect(existsSync(`${logPath("t")}.1`)).toBe(true);
    expect(statSync(logPath("t")).size).toBeLessThan(1_048_576);
  });
});
