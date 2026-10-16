#!/usr/bin/env bun
import { existsSync, readFileSync, watch } from "node:fs";
import { ConfigError, type Job, loadConfig } from "./config";
import { History } from "./history";
import { exec } from "./exec";
import { applySync, defaultCtx, hold, label, planSync, resume, uninstall } from "./launchd";
import { appendLog } from "./log";
import { logPath } from "./paths";
import { runJob } from "./runner";
import { loadSnapshot, type JobView } from "./snapshot";


const hhmm = (t: number | Date | null | undefined) =>
  t == null ? "—" : new Date(t).toTimeString().slice(0, 5);

function statusWord(v: JobView): string {
  if (v.held) return "HOLD";
  if (!v.last) return "NEVER";
  return v.last.status === "ok" || v.last.status === "running" ? "OK" : "FAULT";
}

function findJob(id: string | undefined): Job {
  const job = loadConfig().find((j) => j.id === id);
  if (!job) {
    console.error(`hq: unknown job ${id ?? "(none)"}`);
    process.exit(2);
  }
  return job;
}

async function confirm(question: string): Promise<boolean> {
  process.stdout.write(`${question} [y/N] `);
  for await (const line of console) return line.trim().toLowerCase() === "y";
  return false;
}

async function main(argv: string[]): Promise<number> {
  const [cmd, ...rest] = argv;
  const flag = (f: string) => rest.includes(f);
  const arg = rest.find((a) => !a.startsWith("-"));

  switch (cmd) {
    case undefined: {
      const { startTui } = await import("./tui/start");
      await startTui();
      return 0;
    }
    case "status": {
      const history = new History();
      const snap = await loadSnapshot({ exec, history, now: new Date() });
      if (snap.configError) throw new ConfigError(snap.configError);
      if (flag("--json")) console.log(JSON.stringify(snap.jobs, null, 2));
      else {
        const w = Math.max(4, ...snap.jobs.map((v) => v.job.id.length));
        for (const v of snap.jobs)
          console.log(`${v.job.id.padEnd(w)}  ${statusWord(v).padEnd(5)}  last ${hhmm(v.last?.startedAt)}  next ${hhmm(v.next)}`);
      }
      return 0;
    }
    case "run": {
      const history = new History();
      try {
        return await runJob(findJob(arg), { manual: flag("--manual"), history });
      } catch (e) {
        if (!(e instanceof ConfigError) || !arg) throw e;
        // Under launchd nobody sees stderr: leave the error where hq status and the log will show it.
        const at = Date.now();
        appendLog(arg, `hq: config error: ${e.message}\n`);
        history.finish(history.start(arg, flag("--manual") ? "manual" : "scheduled", at), {
          endedAt: at, exitCode: 1, status: "failed", tail: `config error: ${e.message}`,
        });
        throw e;
      }
    }
    case "logs": {
      const path = logPath(findJob(arg).id);
      let shown = existsSync(path) ? readFileSync(path, "utf8") : "";
      process.stdout.write(shown);
      if (!flag("-f")) return 0;
      watch(path, () => {
        const now = existsSync(path) ? readFileSync(path, "utf8") : "";
        process.stdout.write(now.startsWith(shown) ? now.slice(shown.length) : now); // rotated → reprint
        shown = now;
      });
      return new Promise(() => {}); // follow until Ctrl-C
    }
    case "hold":
      await hold(findJob(arg).id, exec);
      console.log(`${arg} on hold`);
      return 0;
    case "resume":
      await resume(findJob(arg), defaultCtx(), exec);
      console.log(`${arg} resumed`);
      return 0;
    case "sync": {
      const jobs = loadConfig();
      const ctx = defaultCtx();
      const plan = planSync(jobs, ctx);
      if (!plan.add.length && !plan.change.length && !plan.remove.length) {
        console.log("in sync");
        return 0;
      }
      for (const id of plan.add) console.log(`+ ${id}`);
      for (const id of plan.change) console.log(`~ ${id}`);
      for (const id of plan.remove) console.log(`- ${id}  (${label(id)})`);
      if (!flag("--yes") && !(await confirm("Apply?"))) return 1;
      await applySync(plan, jobs, ctx, exec);
      console.log("synced");
      return 0;
    }
    case "uninstall":
      await uninstall(exec);
      console.log("removed hq agents; restored migrated/ plists");
      return 0;
    default:
      console.error("usage: hq [status [--json] | run <job> [--manual] | logs <job> [-f] | hold <job> | resume <job> | sync [--yes] | uninstall]");
      return 2;
  }
}

if (import.meta.main) {
  try {
    process.exitCode = await main(process.argv.slice(2));
  } catch (e) {
    if (!(e instanceof ConfigError)) throw e;
    console.error(`hq: ${e.message}`);
    process.exitCode = 1;
  }
}
