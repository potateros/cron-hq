import { existsSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { ConfigError, type Job, loadConfig } from "./config";
import type { History, RunRow } from "./history";
import { agentState, type AgentState, defaultCtx, type Exec, label, renderPlist } from "./launchd";
import { launchAgentsDir } from "./paths";
import { nextRun } from "./schedule";
import { readPaused } from "./state";

export interface JobView {
  job: Job;
  held: boolean;
  agent: AgentState;
  synced: boolean;
  last: RunRow | null; // newest non-skipped run
  recent: RunRow[]; // newest first, up to 10, including skips
  running: boolean;
  next: Date | null;
}

export interface Snapshot {
  jobs: JobView[];
  configError: string | null;
  at: Date;
}

export async function loadSnapshot(deps: { exec: Exec; history: History; now: Date; previous?: Snapshot }): Promise<Snapshot> {
  let jobs: Job[];
  let configError: string | null = null;
  try {
    jobs = loadConfig();
  } catch (e) {
    if (!(e instanceof ConfigError)) throw e;
    configError = e.message;
    jobs = deps.previous?.jobs.map((v) => v.job) ?? [];
  }

  const held = readPaused();
  const ctx = defaultCtx();
  const views = await Promise.all(
    jobs.map(async (job): Promise<JobView> => {
      const agent = await agentState(job.id, deps.exec);
      const path = join(launchAgentsDir(), `${label(job.id)}.plist`);
      const isHeld = held.has(job.id);
      const manual = job.schedule.kind === "manual";
      const plistMatches = manual || (existsSync(path) && readFileSync(path, "utf8") === renderPlist(job, ctx));
      const recent = deps.history.recent(job.id, 10);
      const lastStart = recent[0] ? new Date(recent[0].startedAt) : null;
      return {
        job,
        held: isHeld,
        agent,
        synced: manual || (plistMatches && (agent.linked || isHeld)),
        last: deps.history.lastReal(job.id),
        recent,
        running: recent[0]?.status === "running",
        next: isHeld ? null : nextRun(job, lastStart, deps.now),
      };
    }),
  );
  return { jobs: views, configError, at: deps.now };
}

export function summarize(s: Snapshot): { systems: number; nominal: number; fault: number; standby: number } {
  const out = { systems: s.jobs.length, nominal: 0, fault: 0, standby: 0 };
  for (const v of s.jobs) {
    if (v.held || !v.last) out.standby++;
    else if (v.last.status === "failed" || v.last.status === "timeout") out.fault++;
    else out.nominal++;
  }
  return out;
}
