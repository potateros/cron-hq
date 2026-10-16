import { copyFileSync, existsSync, mkdirSync, readdirSync, readFileSync, unlinkSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import type { Job } from "./config";
import { hqHome, launchAgentsDir, migratedDir, repoRoot } from "./paths";
import { readPaused, setPaused } from "./state";

export const LABEL_PREFIX = "com.chngu.hq.";

export type Exec = (args: string[]) => Promise<{ code: number; stdout: string; stderr: string }>;

export const realExec: Exec = async (args) => {
  const p = Bun.spawn(["launchctl", ...args], { stdout: "pipe", stderr: "pipe" });
  const [stdout, stderr, code] = await Promise.all([new Response(p.stdout).text(), new Response(p.stderr).text(), p.exited]);
  return { code, stdout, stderr };
};

export const label = (id: string) => `${LABEL_PREFIX}${id}`;
const domain = () => `gui/${process.getuid!()}`;
const target = (id: string) => `${domain()}/${label(id)}`;
const plistPath = (id: string) => join(launchAgentsDir(), `${label(id)}.plist`);

export interface PlistCtx {
  bunPath: string;
  cliPath: string;
  hqHome?: string;
}

export function defaultCtx(): PlistCtx {
  return {
    bunPath: process.execPath,
    cliPath: join(repoRoot(), "src", "cli.ts"),
    ...(process.env.HQ_HOME ? { hqHome: process.env.HQ_HOME } : {}),
  };
}

const esc = (s: string) => s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");

export function renderPlist(job: Job, ctx: PlistCtx): string {
  const launchdLog = join(ctx.hqHome ?? hqHome(), "logs", "_launchd.log");
  const lines = [
    '<?xml version="1.0" encoding="UTF-8"?>',
    '<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">',
    '<plist version="1.0">',
    "<dict>",
    "  <key>Label</key>",
    `  <string>${esc(label(job.id))}</string>`,
    "  <key>ProgramArguments</key>",
    "  <array>",
    ...[ctx.bunPath, ctx.cliPath, "run", job.id].map((a) => `    <string>${esc(a)}</string>`),
    "  </array>",
  ];
  if (job.schedule.kind === "every") {
    lines.push("  <key>StartInterval</key>", `  <integer>${job.schedule.seconds}</integer>`);
  } else {
    lines.push("  <key>StartCalendarInterval</key>", "  <array>");
    for (const t of job.schedule.times) {
      lines.push(
        "    <dict>",
        "      <key>Hour</key>",
        `      <integer>${t.hour}</integer>`,
        "      <key>Minute</key>",
        `      <integer>${t.minute}</integer>`,
        "    </dict>",
      );
    }
    lines.push("  </array>");
  }
  lines.push(
    "  <key>RunAtLoad</key>",
    "  <false/>",
    "  <key>StandardOutPath</key>",
    `  <string>${esc(launchdLog)}</string>`,
    "  <key>StandardErrorPath</key>",
    `  <string>${esc(launchdLog)}</string>`,
  );
  if (ctx.hqHome) {
    lines.push("  <key>EnvironmentVariables</key>", "  <dict>", "    <key>HQ_HOME</key>", `    <string>${esc(ctx.hqHome)}</string>`, "  </dict>");
  }
  lines.push("</dict>", "</plist>", "");
  return lines.join("\n");
}

export interface AgentState {
  linked: boolean;
  running: boolean;
  lastExit: number | null;
}

export async function agentState(id: string, exec: Exec): Promise<AgentState> {
  const r = await exec(["print", target(id)]);
  if (r.code !== 0) return { linked: false, running: false, lastExit: null };
  // Only top-level properties (one tab deep); nested blocks repeat keys like "state".
  const state = /^\tstate = (.+)$/m.exec(r.stdout)?.[1]?.trim();
  const exit = /^\tlast exit code = (-?\d+)/m.exec(r.stdout)?.[1];
  return { linked: true, running: state === "running", lastExit: exit === undefined ? null : Number(exit) };
}

/** ids of installed hq LaunchAgents, from the agents dir. */
function installedIds(): string[] {
  const dir = launchAgentsDir();
  if (!existsSync(dir)) return [];
  return readdirSync(dir)
    .filter((f) => f.startsWith(LABEL_PREFIX) && f.endsWith(".plist"))
    .map((f) => f.slice(LABEL_PREFIX.length, -".plist".length))
    .sort();
}

export interface SyncPlan {
  add: string[];
  change: string[];
  remove: string[];
}

export function planSync(jobs: Job[], ctx: PlistCtx): SyncPlan {
  const installed = new Set(installedIds());
  const plan: SyncPlan = { add: [], change: [], remove: [] };
  for (const job of jobs) {
    if (!installed.has(job.id)) plan.add.push(job.id);
    else if (readFileSync(plistPath(job.id), "utf8") !== renderPlist(job, ctx)) plan.change.push(job.id);
  }
  const wanted = new Set(jobs.map((j) => j.id));
  plan.remove = [...installed].filter((id) => !wanted.has(id));
  return plan;
}

export async function applySync(plan: SyncPlan, jobs: Job[], ctx: PlistCtx, exec: Exec): Promise<void> {
  mkdirSync(launchAgentsDir(), { recursive: true });
  const held = readPaused();
  const byId = new Map(jobs.map((j) => [j.id, j]));
  for (const id of [...plan.add, ...plan.change]) {
    const job = byId.get(id);
    if (!job) throw new Error(`sync plan names unknown job ${id}`);
    writeFileSync(plistPath(id), renderPlist(job, ctx));
    await exec(["bootout", target(id)]); // ignore failure: it may not be loaded
    if (!held.has(id)) await bootstrap(plistPath(id), exec);
  }
  for (const id of plan.remove) {
    await exec(["bootout", target(id)]);
    unlinkSync(plistPath(id));
  }
}

async function bootstrap(path: string, exec: Exec): Promise<void> {
  const r = await exec(["bootstrap", domain(), path]);
  if (r.code !== 0) throw new Error(`launchctl bootstrap ${path} failed (${r.code}): ${r.stderr.trim()}`);
}

export async function hold(id: string, exec: Exec): Promise<void> {
  await exec(["bootout", target(id)]);
  setPaused(id, true);
}

export async function resume(job: Job, ctx: PlistCtx, exec: Exec): Promise<void> {
  mkdirSync(launchAgentsDir(), { recursive: true });
  if (!existsSync(plistPath(job.id))) writeFileSync(plistPath(job.id), renderPlist(job, ctx));
  await bootstrap(plistPath(job.id), exec);
  setPaused(job.id, false);
}

export async function uninstall(exec: Exec): Promise<void> {
  for (const id of installedIds()) {
    await exec(["bootout", target(id)]);
    unlinkSync(plistPath(id));
  }
  const dir = migratedDir();
  if (!existsSync(dir)) return;
  for (const f of readdirSync(dir).filter((f) => f.endsWith(".plist")).sort()) {
    const dest = join(launchAgentsDir(), f);
    copyFileSync(join(dir, f), dest);
    await bootstrap(dest, exec);
  }
}
