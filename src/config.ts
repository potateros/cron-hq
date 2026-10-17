import { readFileSync } from "node:fs";
import { homedir } from "node:os";
import { configPath, expandHome } from "./paths";

export interface QuietWindow {
  start: number; // minutes after midnight
  end: number;
}

export type Schedule =
  | { kind: "every"; seconds: number }
  | { kind: "at"; times: { hour: number; minute: number }[] }
  | { kind: "manual" }; // on demand only: no LaunchAgent, runs when you press R

export interface Job {
  id: string;
  description: string;
  command: string;
  cwd: string;
  schedule: Schedule;
  quiet: QuietWindow | null;
  timeoutSec: number;
  url: string | null; // shown in the detail panel; O opens it
}

export class ConfigError extends Error {
  override name = "ConfigError";
}

const ID_RE = /^[a-z0-9-]+$/;
const KEYS = new Set(["description", "cwd", "command", "every", "at", "manual", "quiet", "timeout", "url"]);
const UNIT = { s: 1, m: 60, h: 3600 } as const;

export function parseDuration(s: string): number {
  const m = /^(\d+)(s|m|h)$/.exec(s);
  if (!m) throw new ConfigError(`invalid duration "${s}" (use e.g. 90s, 30m, 1h)`);
  return Number(m[1]) * UNIT[m[2] as keyof typeof UNIT];
}

function parseClock(s: string): { hour: number; minute: number } {
  const m = /^(\d{2}):(\d{2})$/.exec(s);
  const hour = Number(m?.[1]);
  const minute = Number(m?.[2]);
  if (!m || hour > 23 || minute > 59) throw new ConfigError(`invalid time "${s}" (use HH:MM)`);
  return { hour, minute };
}

export function parseQuiet(s: string): QuietWindow {
  const parts = s.split("-");
  if (parts.length !== 2) throw new ConfigError(`invalid quiet window "${s}" (use HH:MM-HH:MM)`);
  const [a, b] = parts.map((p) => {
    try {
      return parseClock(p.trim());
    } catch {
      throw new ConfigError(`invalid quiet window "${s}" (use HH:MM-HH:MM)`);
    }
  });
  return { start: a!.hour * 60 + a!.minute, end: b!.hour * 60 + b!.minute };
}

function parseJob(id: string, raw: Record<string, unknown>): Job {
  const fail = (msg: string): never => {
    throw new ConfigError(`jobs.${id}: ${msg}`);
  };
  if (!ID_RE.test(id)) fail(`invalid job id "${id}" (use a-z, 0-9 and -)`);
  for (const key of Object.keys(raw)) if (!KEYS.has(key)) fail(`unknown key ${key}`);

  const str = (key: string): string | undefined => {
    const v = raw[key];
    if (v === undefined) return undefined;
    if (typeof v !== "string") fail(`${key} must be a string`);
    return v as string;
  };
  const wrap = <T>(key: string, f: () => T): T => {
    try {
      return f();
    } catch (e) {
      return fail(`${key}: ${(e as Error).message}`);
    }
  };

  const command = str("command");
  if (!command) fail("command is required");

  const every = str("every");
  const at = raw.at;
  const manual = raw.manual;
  if (manual !== undefined && typeof manual !== "boolean") fail("manual must be true or false");

  let schedule: Schedule;
  if (manual === true) {
    if (every !== undefined || at !== undefined) fail("a manual job has no every or at");
    if (raw.quiet !== undefined) fail("quiet has no effect on a manual job");
    schedule = { kind: "manual" };
  } else if (every !== undefined && at !== undefined) {
    return fail("set either every or at, not both");
  } else if (every === undefined && at === undefined) {
    return fail("set every or at (neither is set), or manual = true");
  } else if (every !== undefined) {
    const seconds = wrap("every", () => parseDuration(every));
    if (seconds < 60) fail("every must be at least 60s");
    schedule = { kind: "every", seconds };
  } else {
    if (!Array.isArray(at) || at.length === 0 || !at.every((t) => typeof t === "string"))
      fail('at must be a list of "HH:MM" strings');
    schedule = { kind: "at", times: (at as string[]).map((t) => wrap("at", () => parseClock(t))) };
  }

  const quiet = str("quiet");
  const timeout = str("timeout");
  const url = str("url");
  if (url !== undefined && !/^https?:\/\/\S+$/.test(url)) fail(`url must start with http:// or https://`);
  return {
    id,
    description: str("description") ?? "",
    command: command!,
    cwd: expandHome(str("cwd") ?? homedir()),
    schedule,
    quiet: quiet === undefined ? null : wrap("quiet", () => parseQuiet(quiet)),
    timeoutSec: timeout === undefined ? 600 : wrap("timeout", () => parseDuration(timeout)),
    url: url ?? null,
  };
}

export function parseConfig(text: string): Job[] {
  let doc: Record<string, unknown>;
  try {
    doc = Bun.TOML.parse(text) as Record<string, unknown>;
  } catch (e) {
    throw new ConfigError(`invalid TOML: ${(e as Error).message}`);
  }
  // [stacks.*] is read by the TUI only (src/stacks.ts), so a mistake there never stops a job from running.
  for (const key of Object.keys(doc)) if (key !== "jobs" && key !== "stacks") throw new ConfigError(`unknown top-level key ${key}`);
  const jobs = (doc.jobs ?? {}) as Record<string, Record<string, unknown>>;
  return Object.entries(jobs)
    .map(([id, raw]) => parseJob(id, raw))
    // on-demand jobs first: they're launchers, the rest is the schedule
    .sort((a, b) => Number(b.schedule.kind === "manual") - Number(a.schedule.kind === "manual") || a.id.localeCompare(b.id));
}

export function loadConfig(path = configPath()): Job[] {
  let text: string;
  try {
    text = readFileSync(path, "utf8");
  } catch (e) {
    throw new ConfigError(`cannot read ${path}: ${(e as Error).message}`);
  }
  return parseConfig(text);
}
