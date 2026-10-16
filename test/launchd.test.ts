import { beforeEach, describe, expect, test } from "bun:test";
import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import type { Job } from "../src/config";
import { agentState, applySync, type Exec, hold, label, planSync, renderPlist, resume, uninstall } from "../src/launchd";
import { launchAgentsDir, migratedDir } from "../src/paths";
import { readPaused, setPaused } from "../src/state";
import { tempHome } from "./helpers";

const uid = process.getuid!();
const ctx = { bunPath: "/opt/bun", cliPath: "/repo/src/cli.ts", hqHome: "/hqhome" };
const every: Job = { id: "eink", description: "", command: "x", cwd: "/", quiet: null, timeoutSec: 60, schedule: { kind: "every", seconds: 1800 } };
const at: Job = { ...every, id: "wipe", schedule: { kind: "at", times: [{ hour: 6, minute: 55 }, { hour: 19, minute: 30 }] } };

function fakeExec(respond: (args: string[]) => { code: number; stdout?: string; stderr?: string } = () => ({ code: 0 })) {
  const calls: string[][] = [];
  const exec: Exec = async (args) => {
    calls.push(args);
    const r = respond(args);
    return { code: r.code, stdout: r.stdout ?? "", stderr: r.stderr ?? "" };
  };
  return { exec, calls };
}
const plistFile = (id: string) => join(launchAgentsDir(), `${label(id)}.plist`);

beforeEach(() => {
  tempHome();
  mkdirSync(launchAgentsDir(), { recursive: true });
});

describe("renderPlist", () => {
  test("every", () => {
    const xml = renderPlist(every, ctx);
    expect(xml).toMatchSnapshot();
    expect(xml).toContain("<key>StartInterval</key>\n  <integer>1800</integer>");
    expect(xml).toContain("<string>/opt/bun</string>\n    <string>/repo/src/cli.ts</string>\n    <string>run</string>\n    <string>eink</string>");
    expect(xml).toContain("<key>RunAtLoad</key>\n  <false/>");
    expect(xml).toContain("<string>/hqhome/logs/_launchd.log</string>");
    expect(xml).toContain("<key>HQ_HOME</key>");
  });
  test("at", () => {
    const xml = renderPlist(at, ctx);
    expect(xml).toMatchSnapshot();
    expect(xml).toContain("<key>Hour</key>\n      <integer>6</integer>\n      <key>Minute</key>\n      <integer>55</integer>");
    expect(xml).not.toContain("StartInterval");
  });
  test("escapes XML", () => expect(renderPlist({ ...every, id: "a" }, { ...ctx, cliPath: "/a&b/<c>.ts" })).toContain("/a&amp;b/&lt;c&gt;.ts"));
});

describe("planSync", () => {
  test("empty dir → add all", () => expect(planSync([every, at], ctx)).toEqual({ add: ["eink", "wipe"], change: [], remove: [] }));
  test("detects change, removal, and ignores foreign labels", () => {
    writeFileSync(plistFile("eink"), renderPlist({ ...every, schedule: { kind: "every", seconds: 60 } }, ctx));
    writeFileSync(plistFile("wipe"), renderPlist(at, ctx));
    writeFileSync(plistFile("old"), "<plist/>");
    writeFileSync(join(launchAgentsDir(), "com.chngu.trml.x.plist"), "<plist/>");
    expect(planSync([every, at], ctx)).toEqual({ add: [], change: ["eink"], remove: ["old"] });
  });
});

describe("applySync", () => {
  test("bootout then bootstrap for changes, remove deletes, held jobs are not bootstrapped", async () => {
    writeFileSync(plistFile("old"), "<plist/>");
    setPaused("wipe", true);
    const { exec, calls } = fakeExec();
    await applySync({ add: ["wipe"], change: ["eink"], remove: ["old"] }, [every, at], ctx, exec);
    expect(calls).toEqual([
      ["bootout", `gui/${uid}/${label("wipe")}`],
      ["bootout", `gui/${uid}/${label("eink")}`],
      ["bootstrap", `gui/${uid}`, plistFile("eink")],
      ["bootout", `gui/${uid}/${label("old")}`],
    ]);
    expect(readFileSync(plistFile("wipe"), "utf8")).toBe(renderPlist(at, ctx));
    expect(existsSync(plistFile("old"))).toBe(false);
  });
});

describe("agentState", () => {
  const fixture = readFileSync(join(import.meta.dir, "fixtures", "launchctl-print.txt"), "utf8");
  test("parses top-level state and last exit", async () => {
    const { exec } = fakeExec(() => ({ code: 0, stdout: fixture }));
    expect(await agentState("demo", exec)).toEqual({ linked: true, running: false, lastExit: 0 });
  });
  test("running state", async () => {
    const { exec } = fakeExec(() => ({ code: 0, stdout: fixture.replace("state = not running", "state = running").replace("last exit code = 0", "last exit code = (never exited)") }));
    expect(await agentState("demo", exec)).toEqual({ linked: true, running: true, lastExit: null });
  });
  test("missing service is unlinked, not an error", async () => {
    const { exec } = fakeExec(() => ({ code: 113, stderr: 'Could not find service "com.chngu.hq.demo" in domain for user gui: 501' }));
    expect(await agentState("demo", exec)).toEqual({ linked: false, running: false, lastExit: null });
  });
});

describe("hold / resume / uninstall", () => {
  test("hold boots out and records; resume bootstraps and clears", async () => {
    const { exec, calls } = fakeExec();
    await hold("eink", exec);
    expect(readPaused().has("eink")).toBe(true);
    await resume(every, ctx, exec);
    expect(readPaused().has("eink")).toBe(false);
    expect(existsSync(plistFile("eink"))).toBe(true);
    expect(calls).toEqual([["bootout", `gui/${uid}/${label("eink")}`], ["bootstrap", `gui/${uid}`, plistFile("eink")]]);
  });
  test("uninstall removes hq agents and restores migrated ones", async () => {
    writeFileSync(plistFile("eink"), "<plist/>");
    mkdirSync(migratedDir(), { recursive: true });
    writeFileSync(join(migratedDir(), "com.chngu.trml.reminders.plist"), "<plist>orig</plist>");
    const { exec, calls } = fakeExec();
    await uninstall(exec);
    const restored = join(launchAgentsDir(), "com.chngu.trml.reminders.plist");
    expect(existsSync(plistFile("eink"))).toBe(false);
    expect(readFileSync(restored, "utf8")).toBe("<plist>orig</plist>");
    expect(calls).toEqual([["bootout", `gui/${uid}/${label("eink")}`], ["bootstrap", `gui/${uid}`, restored]]);
  });
});
