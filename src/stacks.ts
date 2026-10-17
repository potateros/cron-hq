import { readFileSync, statSync } from "node:fs";
import { isAbsolute, join } from "node:path";
import { ConfigError } from "./config";
import { configPath, expandHome } from "./paths";

/** A docker compose project listed under [stacks.<name>] in jobs.toml. Read by the TUI only. */
export interface Stack {
  name: string;
  dir: string;
  files: string[]; // absolute; empty = compose's default lookup in dir
  envFile: string | null;
}

const KEYS = new Set(["dir", "files", "env_file"]);
const NAME_RE = /^[a-z0-9-]+$/;

export function parseStacks(text: string): Stack[] {
  let doc: Record<string, unknown>;
  try {
    doc = Bun.TOML.parse(text) as Record<string, unknown>;
  } catch (e) {
    throw new ConfigError(`invalid TOML: ${(e as Error).message}`);
  }
  const stacks = (doc.stacks ?? {}) as Record<string, Record<string, unknown>>;
  return Object.entries(stacks).map(([name, raw]) => {
    const fail = (msg: string): never => {
      throw new ConfigError(`stacks.${name}: ${msg}`);
    };
    if (!NAME_RE.test(name)) fail(`invalid stack name "${name}" (use a-z, 0-9 and -)`);
    for (const key of Object.keys(raw)) if (!KEYS.has(key)) fail(`unknown key ${key}`);
    if (typeof raw.dir !== "string") fail("dir is required");
    const dir = expandHome(raw.dir as string);
    const abs = (p: string) => (isAbsolute(expandHome(p)) ? expandHome(p) : join(dir, p));
    const files = raw.files ?? [];
    if (!Array.isArray(files) || !files.every((f) => typeof f === "string")) fail("files must be a list of paths");
    if (raw.env_file !== undefined && typeof raw.env_file !== "string") fail("env_file must be a string");
    return {
      name,
      dir,
      files: (files as string[]).map(abs),
      envFile: raw.env_file === undefined ? null : abs(raw.env_file as string),
    };
  });
}

export function loadStacks(path = configPath()): Stack[] {
  let text: string;
  try {
    text = readFileSync(path, "utf8");
  } catch (e) {
    throw new ConfigError(`cannot read ${path}: ${(e as Error).message}`);
  }
  return parseStacks(text);
}

// ─── docker ──────────────────────────────────────────────────────────────────

/** `timeoutMs`: kill docker after this long. Polls use the default; start/stop pass null (a pull can take minutes). */
export type DockerExec = (args: string[], timeoutMs?: number | null) => Promise<{ code: number; stdout: string; stderr: string }>;

export const realDocker: DockerExec = async (args, timeoutMs = 15_000) => {
  const p = Bun.spawn(["docker", ...args], { stdout: "pipe", stderr: "pipe", env: process.env });
  const kill = timeoutMs === null ? undefined : setTimeout(() => p.kill(), timeoutMs);
  const [stdout, stderr, code] = await Promise.all([new Response(p.stdout).text(), new Response(p.stderr).text(), p.exited]);
  clearTimeout(kill);
  return { code, stdout, stderr };
};

/** `docker compose` with the stack's project dir, env file and compose files. */
export function composeArgs(s: Stack, ...rest: string[]): string[] {
  const args = ["compose", "--project-directory", s.dir];
  if (s.envFile) args.push("--env-file", s.envFile);
  for (const f of s.files) args.push("-f", f);
  return [...args, ...rest];
}

export type ServiceState = "running" | "restarting" | "paused" | "exited" | "created" | "dead" | "missing";

export interface ServiceView {
  stack: string;
  service: string;
  container: string | null;
  image: string;
  ports: number[]; // published host ports, first = the web UI
  state: ServiceState; // "missing" = in the compose file but no container exists
  health: string; // "", "healthy", "unhealthy", "starting"
  status: string; // docker's own wording, e.g. "Up 39 hours"
  exitCode: number;
  cpu: number | null; // percent of one core, from docker stats
  mem: number | null; // bytes
}

export interface StackView {
  stack: Stack;
  services: ServiceView[];
  error: string | null;
}

export interface Usage {
  cpu: number;
  mem: number;
}

const UNITS: Record<string, number> = {
  B: 1, KB: 1e3, MB: 1e6, GB: 1e9, TB: 1e12, KIB: 1024, MIB: 1024 ** 2, GIB: 1024 ** 3, TIB: 1024 ** 4,
};

/** "38.67MiB" → bytes. */
export function parseBytes(s: string): number | null {
  const m = /^([\d.]+)\s*([a-zA-Z]+)$/.exec(s.trim());
  const unit = m ? UNITS[m[2]!.toUpperCase()] : undefined;
  return m && unit ? Number(m[1]) * unit : null;
}

/** NDJSON from `docker stats --no-stream --format '{{json .}}'` → usage by container name. */
export function parseStats(stdout: string): Map<string, Usage> {
  const out = new Map<string, Usage>();
  for (const row of parseJsonLines(stdout)) {
    const cpu = Number.parseFloat(String(row.CPUPerc ?? ""));
    const mem = parseBytes(String(row.MemUsage ?? "").split("/")[0] ?? "");
    if (typeof row.Name === "string" && !Number.isNaN(cpu) && mem !== null) out.set(row.Name, { cpu, mem });
  }
  return out;
}

/** Compose prints either one JSON array or one object per line, depending on version. */
function parseJsonLines(stdout: string): Record<string, unknown>[] {
  const t = stdout.trim();
  if (!t) return [];
  if (t.startsWith("[")) return JSON.parse(t);
  return t.split("\n").filter((l) => l.trim()).map((l) => JSON.parse(l));
}

interface Declared {
  image: string;
  containerName: string | null;
  ports: number[];
}

/** `compose config --format json` → declared services, in file order. */
export function parseComposeConfig(stdout: string): Map<string, Declared> {
  const doc = JSON.parse(stdout) as { services?: Record<string, Record<string, unknown>> };
  const out = new Map<string, Declared>();
  for (const [name, svc] of Object.entries(doc.services ?? {})) {
    const ports = ((svc.ports as { published?: string | number; protocol?: string }[] | null) ?? [])
      .filter((p) => p.published !== undefined && (p.protocol ?? "tcp") === "tcp")
      .map((p) => Number(p.published))
      .filter((n) => Number.isInteger(n) && n > 0);
    out.set(name, {
      image: String(svc.image ?? ""),
      containerName: typeof svc.container_name === "string" ? svc.container_name : null,
      ports: [...new Set(ports)],
    });
  }
  return out;
}

const lastLine = (s: string) => s.trim().split("\n").filter(Boolean).at(-1) ?? "";

/** `compose config` is slow-ish and only changes with the files, so cache it by their mtimes. */
const configCache = new Map<string, { key: string; declared: Map<string, Declared> }>();

function filesKey(s: Stack): string {
  const files = s.files.length ? s.files : ["compose.yaml", "compose.yml", "docker-compose.yml", "docker-compose.yaml"].map((f) => join(s.dir, f));
  return [...files, ...(s.envFile ? [s.envFile] : [join(s.dir, ".env")])]
    .map((f) => {
      try {
        return `${f}@${statSync(f).mtimeMs}`;
      } catch {
        return `${f}@-`;
      }
    })
    .join("|");
}

export async function loadStackView(s: Stack, docker: DockerExec, usage: Map<string, Usage>): Promise<StackView> {
  try {
    const key = filesKey(s);
    let declared = configCache.get(s.name)?.key === key ? configCache.get(s.name)!.declared : undefined;
    if (!declared) {
      const cfg = await docker(composeArgs(s, "config", "--format", "json"));
      if (cfg.code !== 0) return { stack: s, services: [], error: lastLine(cfg.stderr) || `compose config exited ${cfg.code}` };
      declared = parseComposeConfig(cfg.stdout);
      configCache.set(s.name, { key, declared });
    }
    const ps = await docker(composeArgs(s, "ps", "-a", "--format", "json"));
    if (ps.code !== 0) return { stack: s, services: [], error: lastLine(ps.stderr) || `compose ps exited ${ps.code}` };
    const live = new Map(parseJsonLines(ps.stdout).map((r) => [String(r.Service), r]));

    const services = [...declared.entries()].map(([service, d]): ServiceView => {
      const r = live.get(service);
      const container = r ? String(r.Name) : null;
      const u = container ? usage.get(container) : undefined;
      return {
        stack: s.name,
        service,
        container,
        image: d.image,
        ports: d.ports,
        state: r ? (String(r.State) as ServiceState) : "missing",
        health: r ? String(r.Health ?? "") : "",
        status: r ? String(r.Status ?? "") : "no container",
        exitCode: r ? Number(r.ExitCode ?? 0) : 0,
        cpu: u?.cpu ?? null,
        mem: u?.mem ?? null,
      };
    });
    return { stack: s, services, error: null };
  } catch (e) {
    return { stack: s, services: [], error: (e as Error).message };
  }
}

export async function loadUsage(docker: DockerExec): Promise<Map<string, Usage>> {
  const r = await docker(["stats", "--no-stream", "--format", "{{json .}}"]);
  if (r.code !== 0) throw new Error(lastLine(r.stderr) || `docker stats exited ${r.code}`);
  return parseStats(r.stdout);
}

export const isUp = (v: ServiceView) => v.state === "running" || v.state === "restarting";

/** Running but unhealthy, restarting, or exited with an error. */
export function isServiceFault(v: ServiceView): boolean {
  if (v.state === "restarting" || v.state === "dead" || v.health === "unhealthy") return true;
  return v.state === "exited" && v.exitCode !== 0 && v.exitCode !== 137 && v.exitCode !== 143; // 137/143 = stopped by us
}

export const serviceUrl = (v: ServiceView) => (v.ports[0] ? `http://localhost:${v.ports[0]}` : null);

export function totals(views: ServiceView[]): { up: number; all: number; cpu: number; mem: number } {
  let up = 0, cpu = 0, mem = 0;
  for (const v of views) {
    if (isUp(v)) up++;
    cpu += v.cpu ?? 0;
    mem += v.mem ?? 0;
  }
  return { up, all: views.length, cpu, mem };
}

export function formatBytes(n: number): string {
  if (n >= 1024 ** 3) return `${(n / 1024 ** 3).toFixed(1)}G`;
  if (n >= 1024 ** 2) return `${Math.round(n / 1024 ** 2)}M`;
  if (n >= 1024) return `${Math.round(n / 1024)}K`;
  return `${Math.round(n)}B`;
}
