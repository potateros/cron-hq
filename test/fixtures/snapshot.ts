import type { Job } from "../../src/config";
import type { RunRow, Status } from "../../src/history";
import type { JobView, Snapshot } from "../../src/snapshot";

export const NOW = new Date(2026, 9, 16, 17, 32, 8);
const t = (h: number, m: number) => new Date(2026, 9, 16, h, m).getTime();

let nextId = 1;
const run = (job: string, status: Status, startedAt: number, secs = 41, exitCode: number | null = 0, tail = ""): RunRow => ({
  id: nextId++, job, trigger: "scheduled", startedAt, endedAt: startedAt + secs * 1000, exitCode, status,
  reason: status === "skipped" ? "quiet" : null, tail,
});
const job = (id: string, over: Partial<Job> = {}): Job => ({
  id, description: "", command: "true", cwd: "/tmp", quiet: null, timeoutSec: 600, url: null,
  schedule: { kind: "every", seconds: 1800 }, ...over,
});
const linked = { linked: true, running: false, lastExit: 0 };

const einkRuns = [
  run("eink-weather", "ok", t(17, 30), 41, 0, "connecting\npushed (battery 2921 mV, clock drift -3s), fallback armed for midnight"),
  run("eink-weather", "failed", t(17, 0), 50, 1),
  run("eink-weather", "skipped", t(16, 30), 0, null),
];
const tsRuns = [run("tailscale-watchdog", "failed", t(17, 5), 9, 1, "tailscale: not running")];

export const views: JobView[] = [
  {
    job: job("eink-weather", { description: "Punggol weather → nRF e-ink", command: "uv run python -m epd.cli weather --push",
      cwd: "/Users/x/Developer/potateros/nrf5-eink", quiet: { start: 1380, end: 420 }, timeoutSec: 300 }),
    held: false, agent: linked, synced: true, last: einkRuns[0]!, recent: einkRuns, running: false,
    next: new Date(NOW.getTime() + 1_672_000),
  },
  {
    job: job("tailscale-watchdog", { schedule: { kind: "every", seconds: 3600 } }),
    held: false, agent: { ...linked, lastExit: 1 }, synced: true, last: tsRuns[0]!, recent: tsRuns, running: false,
    next: new Date(NOW.getTime() + 1_972_000),
  },
  {
    job: job("eink-wipe", { schedule: { kind: "at", times: [{ hour: 6, minute: 55 }] } }),
    held: true, agent: { linked: false, running: false, lastExit: null }, synced: true, last: null, recent: [], running: false, next: null,
  },
  {
    job: job("trml-reminders", { schedule: { kind: "every", seconds: 300 } }),
    held: false, agent: { linked: false, running: false, lastExit: null }, synced: false,
    last: run("trml-reminders", "ok", t(17, 30), 2), recent: [], running: false, next: new Date(NOW.getTime() + 172_000),
  },
];

export const snapshot: Snapshot = { jobs: views, configError: null, at: NOW };
