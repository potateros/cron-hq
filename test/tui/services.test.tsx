import { beforeEach, describe, expect, test } from "bun:test";
import { writeFileSync } from "node:fs";
import { join } from "node:path";
import { render } from "ink-testing-library";
import { History } from "../../src/history";
import type { Exec } from "../../src/launchd";
import type { DockerExec, Stack } from "../../src/stacks";
import { App } from "../../src/tui/App";
import { tempHome } from "../helpers";

const tick = (ms = 80) => new Promise((r) => setTimeout(r, ms));
const KEY = { down: "\u001B[B", tab: "\t" };
const exec: Exec = async () => ({ code: 113, stdout: "", stderr: "Could not find service" });
const stack: Stack = { name: "lab", dir: "/lab", files: [], envFile: null };

const CONFIG = JSON.stringify({ services: {
  jellyfin: { image: "jellyfin/jellyfin", ports: [{ target: 8096, published: "8096" }] },
  radarr: { image: "lscr.io/linuxserver/radarr", ports: [{ target: 7878, published: "7878" }] },
} });

let calls: string[][];
let timeouts: (number | null | undefined)[];
let opened: string[];
let running: Set<string>;
const docker: DockerExec = async (args, timeoutMs) => {
  calls.push(args);
  timeouts.push(timeoutMs);
  const rest = args.slice(3); // after compose --project-directory /lab
  if (rest[0] === "config") return { code: 0, stdout: CONFIG, stderr: "" };
  if (rest[0] === "ps")
    return { code: 0, stdout: [...running].map((s) => JSON.stringify({ Service: s, Name: s, State: "running", Status: "Up 3 hours", ExitCode: 0 })).join("\n"), stderr: "" };
  if (args[0] === "stats")
    return { code: 0, stdout: [...running].map((s) => JSON.stringify({ Name: s, CPUPerc: "62.0%", MemUsage: "1.5GiB / 8GiB" })).join("\n"), stderr: "" };
  if (rest[0] === "stop") (rest[1] ? [rest[1]] : [...running]).forEach((s) => running.delete(s));
  if (rest[0] === "up") (rest[2] ? [rest[2]] : ["jellyfin", "radarr"]).forEach((s) => running.add(s));
  return { code: 0, stdout: "", stderr: "" };
};

beforeEach(() => {
  const dir = tempHome();
  process.env.HQ_CONFIG = join(dir, "jobs.toml");
  writeFileSync(process.env.HQ_CONFIG, '[jobs.tool]\ncommand = "true"\nmanual = true\nurl = "http://localhost:8742/"\n');
  calls = [];
  timeouts = [];
  opened = [];
  running = new Set(["jellyfin"]);
});

async function app() {
  const r = render(
    <App deps={{ exec, history: new History(), spawnCli: () => {}, docker, stacks: () => [stack], open: (u) => opened.push(u) }} />,
  );
  await tick(200);
  return r;
}
const actions = () => calls.filter((c) => ["stop", "up"].includes(c[3]!)).map((c) => c.slice(3));

describe("services", () => {
  test("jobs view shows a docker summary line, the job url, and O opens it", async () => {
    const r = await app();
    expect(r.lastFrame()).toContain("SERVICES 1/2 up · cpu 62.0% · mem 1.5G");
    expect(r.lastFrame()).toContain("url       http://localhost:8742/");
    r.stdin.write("o");
    await tick();
    expect(opened).toEqual(["http://localhost:8742/"]);
    r.unmount();
  });

  test("Tab lists services with state, usage and port", async () => {
    const r = await app();
    r.stdin.write(KEY.tab);
    await tick();
    const f = r.lastFrame()!;
    expect(f).toContain("◢ H Q · SERVICES");
    expect(f).toContain("UP 1/2");
    expect(f).toContain("LAB · 1/2 up");
    expect(f).toMatch(/◉ {2}JELLYFIN\s+up 3h\s+62\.0%\s+1\.5G\s+:8096/);
    expect(f).toMatch(/○ {2}RADARR\s+not created/);
    expect(f).toContain("url       http://localhost:8096");
    r.stdin.write(KEY.tab);
    await tick();
    expect(r.lastFrame()).toContain("SYSTEMS");
    r.unmount();
  });

  test("O opens the service, X stops or starts it", async () => {
    const r = await app();
    r.stdin.write(KEY.tab);
    await tick();
    r.stdin.write("o");
    await tick();
    expect(opened).toEqual(["http://localhost:8096"]);
    r.stdin.write("x");
    await tick(150);
    expect(actions()).toEqual([["stop", "jellyfin"]]);
    // polls keep docker's default timeout; start/stop run uncapped (a pull can take minutes)
    expect(timeouts[calls.findIndex((c) => c[3] === "stop")]).toBeNull();
    expect(timeouts[calls.findIndex((c) => c[3] === "ps")]).toBeUndefined();
    r.stdin.write(KEY.down);
    await tick();
    r.stdin.write("x");
    await tick(150);
    expect(actions().at(-1)).toEqual(["up", "-d", "radarr"]);
    expect(r.lastFrame()).toMatch(/RADARR\s+up 3h/);
    r.unmount();
  });

  test("Z asks before stopping the whole stack", async () => {
    running = new Set(["jellyfin", "radarr"]);
    const r = await app();
    r.stdin.write(KEY.tab);
    await tick();
    r.stdin.write("z");
    await tick();
    expect(r.lastFrame()).toContain("stop 2 services in LAB?  Y confirm");
    r.stdin.write("n");
    await tick();
    expect(actions()).toEqual([]);
    r.stdin.write("z");
    await tick();
    r.stdin.write("y");
    await tick(150);
    expect(actions()).toEqual([["stop"]]);
    expect(r.lastFrame()).toContain("UP 0/2");
    r.unmount();
  });

  test("H shows services help in the services view, jobs help in the jobs view", async () => {
    const r = await app();
    r.stdin.write("h");
    await tick();
    expect(r.lastFrame()).toContain("JOB STATUS");
    expect(r.lastFrame()).not.toContain("O · X · Z");
    r.stdin.write("\u001B");
    await tick();
    r.stdin.write(KEY.tab);
    await tick();
    r.stdin.write("h");
    await tick();
    expect(r.lastFrame()).toContain("O · X · Z");
    expect(r.lastFrame()).not.toContain("JOB STATUS");
    r.unmount();
  });

  test("a failed compose command shows as an error", async () => {
    const failing: DockerExec = async (args) => (args.includes("up") ? { code: 1, stdout: "", stderr: "Error: drive not mounted\n" } : docker(args));
    const r = render(<App deps={{ exec, history: new History(), spawnCli: () => {}, docker: failing, stacks: () => [stack] }} />);
    await tick(200);
    r.stdin.write(KEY.tab);
    await tick();
    r.stdin.write(KEY.down);
    await tick();
    r.stdin.write("x");
    await tick(150);
    expect(r.lastFrame()).toContain("ERROR — lab: up -d radarr — Error: drive not mounted");
    r.unmount();
  });
});
