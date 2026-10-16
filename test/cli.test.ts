import { beforeEach, describe, expect, test } from "bun:test";
import { existsSync, readFileSync, writeFileSync } from "node:fs";
import { History } from "../src/history";
import { lockPath, logPath } from "../src/paths";
import { join } from "node:path";
import { summarize, type Snapshot } from "../src/snapshot";
import { tempHome } from "./helpers";

const CLI = join(import.meta.dir, "..", "src", "cli.ts");
let dir: string;
let env: Record<string, string>;

beforeEach(() => {
  dir = tempHome();
  const config = join(dir, "jobs.toml");
  writeFileSync(config, `
[jobs.ok]
command = "echo fine"
every = "5m"

[jobs.bad]
command = "exit 4"
every = "1h"
`);
  env = { ...process.env, HQ_CONFIG: config, HQ_FAKE_LAUNCHCTL: "1" } as Record<string, string>;
});

async function hq(...args: string[]) {
  const p = Bun.spawn([process.execPath, CLI, ...args], { env, stdout: "pipe", stderr: "pipe", stdin: "ignore" });
  const [stdout, stderr, code] = await Promise.all([new Response(p.stdout).text(), new Response(p.stderr).text(), p.exited]);
  return { stdout, stderr, code };
}

describe("cli", () => {
  test("status --json lists jobs", async () => {
    const r = await hq("status", "--json");
    expect(r.code).toBe(0);
    expect(JSON.parse(r.stdout).map((v: { job: { id: string } }) => v.job.id)).toEqual(["bad", "ok"]);
  });

  test("run --manual returns the job's exit code and shows up in status", async () => {
    expect((await hq("run", "bad", "--manual")).code).toBe(4);
    expect((await hq("run", "ok", "--manual")).code).toBe(0);
    const s = (await hq("status")).stdout;
    expect(s).toMatch(/^bad\s+FAULT\s+last \d\d:\d\d\s+next/m);
    expect(s).toMatch(/^ok\s+OK\s+last \d\d:\d\d/m);
  });

  test("run unknown job exits 2", async () => {
    const r = await hq("run", "nope");
    expect(r.code).toBe(2);
    expect(r.stderr).toContain("unknown job nope");
  });

  test("hold and resume toggle HOLD", async () => {
    await hq("hold", "ok");
    expect((await hq("status")).stdout).toMatch(/^ok\s+HOLD\s/m);
    await hq("resume", "ok");
    expect((await hq("status")).stdout).toMatch(/^ok\s+NEVER\s/m);
  });

  test("sync --yes writes plists and calls launchctl", async () => {
    const r = await hq("sync", "--yes");
    expect(r.code).toBe(0);
    expect(r.stdout).toContain("+ bad");
    expect(existsSync(join(process.env.HQ_LAUNCH_AGENTS_DIR!, "com.chngu.hq.ok.plist"))).toBe(true);
    expect(readFileSync(join(process.env.HQ_HOME!, "launchctl.calls"), "utf8")).toContain("bootstrap");
    expect((await hq("sync", "--yes")).stdout).toContain("in sync");
  });

  test("logs prints the job log", async () => {
    await hq("run", "ok", "--manual");
    const r = await hq("logs", "ok");
    expect(r.stdout).toContain("fine");
    expect(r.stdout).toContain("── RUN 0001");
  });

  test("SIGTERM to a running job finishes the run, kills the job and frees the lock", async () => {
    const pidFile = join(dir, "pid");
    writeFileSync(env.HQ_CONFIG!, `[jobs.slow]\ncommand = "sleep 20 & echo $! > ${pidFile}; wait"\nevery = "5m"\n`);
    const p = Bun.spawn([process.execPath, CLI, "run", "slow", "--manual"], { env, stdout: "ignore", stderr: "ignore", stdin: "ignore" });
    for (let i = 0; i < 100 && !existsSync(pidFile); i++) await Bun.sleep(50);
    const pid = Number(readFileSync(pidFile, "utf8"));
    p.kill("SIGTERM");
    await p.exited;
    await Bun.sleep(100);
    expect(() => process.kill(pid, 0)).toThrow();
    expect(existsSync(lockPath("slow"))).toBe(false);
    const row = new History().lastReal("slow")!;
    expect(row.status).toBe("failed");
    expect(row.tail).toContain("killed by SIGTERM");
    expect(readFileSync(logPath("slow"), "utf8")).toMatch(/── END \d{4} · FAULT · /);
  }, 15_000);

  test("run with a broken config records a fault for that job", async () => {
    writeFileSync(env.HQ_CONFIG!, "[jobs.ok]\ncolour = 1\n");
    const r = await hq("run", "ok");
    expect(r.code).toBe(1);
    expect(readFileSync(logPath("ok"), "utf8")).toContain("jobs.ok");
    const row = new History().lastReal("ok")!;
    expect(row.status).toBe("failed");
    expect(row.tail).toContain("config error");
  });

  test("broken config is reported, not a stack trace", async () => {
    writeFileSync(env.HQ_CONFIG!, "[jobs.x]\ncolour = 1\n");
    const r = await hq("status");
    expect(r.code).toBe(1);
    expect(r.stderr).toContain("hq: jobs.x");
    expect(r.stderr).not.toContain("    at ");
  });
});

describe("summarize", () => {
  const view = (id: string, held: boolean, status: string | null) => ({
    job: { id }, held, synced: true, running: false, next: null, recent: [],
    agent: { linked: true, running: false, lastExit: null },
    last: status ? { status } : null,
  });
  test("counts by rules", () => {
    const snap = {
      at: new Date(), configError: null,
      jobs: [view("a", false, "ok"), view("b", false, "failed"), view("c", false, "timeout"), view("d", true, "ok"), view("e", false, null)],
    } as unknown as Snapshot;
    expect(summarize(snap)).toEqual({ systems: 5, nominal: 1, fault: 2, standby: 2 });
  });
});
