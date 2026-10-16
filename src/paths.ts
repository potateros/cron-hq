import { homedir } from "node:os";
import { dirname, join, resolve } from "node:path";

export function expandHome(p: string): string {
  if (p === "~") return homedir();
  if (p.startsWith("~/")) return join(homedir(), p.slice(2));
  return p;
}

export function repoRoot(): string {
  return resolve(dirname(import.meta.dir));
}

export function hqHome(): string {
  return process.env.HQ_HOME || join(homedir(), "Library", "Application Support", "hq");
}

export const dbPath = () => join(hqHome(), "history.db");
export const statePath = () => join(hqHome(), "state.json");
export const logPath = (id: string) => join(hqHome(), "logs", `${id}.log`);
export const lockPath = (id: string) => join(hqHome(), "locks", `${id}.lock`);

export function launchAgentsDir(): string {
  return process.env.HQ_LAUNCH_AGENTS_DIR || join(homedir(), "Library", "LaunchAgents");
}

export function configPath(): string {
  return process.env.HQ_CONFIG || join(repoRoot(), "jobs.toml");
}

export const migratedDir = () => process.env.HQ_MIGRATED_DIR || join(repoRoot(), "migrated");
