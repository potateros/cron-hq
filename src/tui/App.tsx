import { Box, Text, useApp, useInput } from "ink";
import { useEffect, useRef, useState } from "react";
import { ConfigError, loadConfig } from "../config";
import type { History } from "../history";
import { defaultCtx, type Exec, planSync, type SyncPlan } from "../launchd";
import { loadSnapshot, type Snapshot } from "../snapshot";
import {
  composeArgs, type DockerExec, isUp, loadStacks, loadStackView, loadUsage, realDocker, type ServiceView, serviceUrl,
  type Stack, type StackView, totals, type Usage, formatBytes,
} from "../stacks";
import { Dashboard } from "./Dashboard";
import { HelpView } from "./HelpView";
import { LogView } from "./LogView";
import { type Pending, serviceKey, ServicesView } from "./ServicesView";
import { color } from "./theme";

export interface AppDeps {
  exec: Exec;
  history: History;
  spawnCli: (args: string[]) => void;
  clock?: () => Date;
  onExit?: () => void;
  docker?: DockerExec;
  open?: (url: string) => void;
  stacks?: () => Stack[];
}

type Mode =
  | { kind: "dash" }
  | { kind: "help" }
  | { kind: "log"; id: string }
  | { kind: "sync"; plan: SyncPlan | null; error?: string }
  | { kind: "stack"; view: StackView; start: boolean };

const openUrl = (url: string) => void Bun.spawn(["open", url], { stdio: ["ignore", "ignore", "ignore"] }).unref();
const lastLine = (s: string) => s.trim().split("\n").filter(Boolean).at(-1) ?? "";
/** A start/stop that never shows up in docker is forgotten after this long. */
const PENDING_MS = 120_000;

export function App({ deps }: { deps: AppDeps }) {
  const clock = deps.clock ?? (() => new Date());
  const { exit } = useApp();
  const [now, setNow] = useState(clock);
  const [snap, setSnap] = useState<Snapshot | null>(null);
  const [selected, setSelected] = useState(0);
  const [mode, setMode] = useState<Mode>({ kind: "dash" });
  const prev = useRef<Snapshot | undefined>(undefined);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [view, setView] = useState<"jobs" | "services">("jobs");
  const [stackViews, setStackViews] = useState<StackView[] | null>(null);
  const [svcError, setSvcError] = useState<string | null>(null);
  const [actionError, setActionError] = useState<string | null>(null); // stays until the next action
  const [svcSelected, setSvcSelected] = useState(0);
  const [pending, setPending] = useState<ReadonlyMap<string, { kind: "starting" | "stopping"; at: number }>>(new Map());
  const reloadServices = useRef<() => Promise<void>>(async () => {});
  const viewRef = useRef(view);
  viewRef.current = view;
  const docker = deps.docker ?? realDocker;
  const open = deps.open ?? openUrl;

  useEffect(() => {
    let alive = true;
    const load = async () => {
      try {
        const s = await loadSnapshot({ exec: deps.exec, history: deps.history, now: clock(), previous: prev.current });
        if (!alive) return;
        prev.current = s;
        setSnap(s);
        setLoadError(null);
      } catch (e) {
        // Keep the last good picture; an unhandled rejection here would exit the whole TUI.
        if (alive) setLoadError((e as Error).message);
      }
    };
    void load();
    const reload = setInterval(load, 2000);
    const tick = setInterval(() => setNow(clock()), 1000);
    return () => {
      alive = false;
      clearInterval(reload);
      clearInterval(tick);
    };
  }, []);

  // Docker is polled separately from the jobs: `docker stats` alone takes ~2 s. Behind the jobs view
  // only the one-line summary needs it, so poll a lot less there to keep hq's own load down.
  useEffect(() => {
    let alive = true;
    let busy = false;
    let usage = new Map<string, Usage>();
    const stacks = () => (deps.stacks ?? loadStacks)();
    const load = async () => {
      if (busy) return;
      busy = true;
      try {
        const list = stacks();
        const views = await Promise.all(list.map((s) => loadStackView(s, docker, usage)));
        if (!alive) return;
        setStackViews(views);
        setSvcError(null);
        const byKey = new Map(views.flatMap((v) => v.services).map((v) => [serviceKey(v), v]));
        setPending((prev) => {
          const next = new Map(prev);
          for (const [k, p] of prev) {
            const v = byKey.get(k);
            const done = !v || (p.kind === "starting" ? isUp(v) : !isUp(v));
            if (done || Date.now() - p.at > PENDING_MS) next.delete(k);
          }
          return next.size === prev.size ? prev : next;
        });
      } catch (e) {
        if (!alive) return;
        setSvcError((e as Error).message);
        setStackViews((v) => v ?? []);
      } finally {
        busy = false;
      }
    };
    let statsBusy = false;
    const stats = async () => {
      if (statsBusy) return;
      statsBusy = true;
      try {
        if (stacks().length) {
          usage = await loadUsage(docker);
          await load();
        }
      } catch {
        // docker down or stats failing: the ps-based rows still show state, just no CPU/MEM
      } finally {
        statsBusy = false;
      }
    };
    reloadServices.current = load;
    void load().then(stats);
    let ticks = 0;
    const t1 = setInterval(() => {
      ticks++;
      const services = viewRef.current === "services";
      if (services || ticks % 5 === 0) void load(); // 3 s, or 15 s behind the jobs view
      if (services ? ticks % 2 === 0 : ticks % 10 === 0) void stats(); // 6 s, or 30 s
    }, 3000);
    return () => {
      alive = false;
      clearInterval(t1);
    };
  }, []);

  const jobs = snap?.jobs ?? [];
  const current = jobs[Math.min(selected, jobs.length - 1)];
  const services = (stackViews ?? []).flatMap((s) => s.services);
  const svcIndex = Math.min(svcSelected, Math.max(0, services.length - 1));
  const svc: ServiceView | undefined = services[svcIndex];
  const stackOf = (v: ServiceView) => stackViews?.find((s) => s.stack.name === v.stack);

  /** Run a compose command in the background; rows show starting…/stopping… until docker agrees. */
  const compose = (s: Stack, args: string[], keys: string[], kind: "starting" | "stopping") => {
    setActionError(null);
    setPending((prev) => new Map([...prev, ...keys.map((k) => [k, { kind, at: Date.now() }] as const)]));
    docker(composeArgs(s, ...args), null)
      .then((r) => {
        if (r.code !== 0) throw new Error(lastLine(r.stderr) || `exit ${r.code}`);
      })
      .catch((e: Error) => {
        setActionError(`${s.name}: ${args.join(" ")} — ${e.message}`);
        setPending((prev) => new Map([...prev].filter(([k]) => !keys.includes(k))));
      })
      .finally(() => void reloadServices.current());
  };

  useInput((input, key) => {
    const k = input.toLowerCase();
    if (mode.kind === "log") {
      if (key.escape) setMode({ kind: "dash" });
      return;
    }
    if (mode.kind === "help") {
      if (key.escape || k === "h") setMode({ kind: "dash" });
      return;
    }
    if (mode.kind === "sync") {
      if (k === "y" && mode.plan) deps.spawnCli(["sync", "--yes"]);
      setMode({ kind: "dash" });
      return;
    }
    if (mode.kind === "stack") {
      if (k === "y") {
        const { stack, services: list } = mode.view;
        const keys = list.filter((v) => isUp(v) !== mode.start).map(serviceKey);
        compose(stack, mode.start ? ["up", "-d"] : ["stop"], keys, mode.start ? "starting" : "stopping");
      }
      setMode({ kind: "dash" });
      return;
    }
    if (key.tab) {
      setView((v) => (v === "jobs" ? "services" : "jobs"));
      void reloadServices.current();
      return;
    }
    if (k === "h") return setMode({ kind: "help" });
    if (k === "q") {
      deps.onExit?.();
      return exit();
    }
    if (view === "services") {
      if (key.upArrow) setSvcSelected(Math.max(0, svcIndex - 1));
      else if (key.downArrow) setSvcSelected(Math.min(services.length - 1, svcIndex + 1));
      else if (k === "o" && svc && serviceUrl(svc)) open(serviceUrl(svc)!);
      else if (k === "x" && svc && !pending.has(serviceKey(svc))) {
        const s = stackOf(svc);
        if (s) {
          const up = isUp(svc);
          compose(s.stack, up ? ["stop", svc.service] : ["up", "-d", svc.service], [serviceKey(svc)], up ? "stopping" : "starting");
        }
      } else if (k === "z" && svc) {
        const s = stackOf(svc);
        if (s) setMode({ kind: "stack", view: s, start: !s.services.some(isUp) });
      }
      return;
    }
    if (key.upArrow) setSelected((i) => Math.max(0, i - 1));
    else if (key.downArrow) setSelected((i) => Math.min(jobs.length - 1, i + 1));
    else if (key.return && current) setMode({ kind: "log", id: current.job.id });
    else if (k === "r" && current) {
      deps.spawnCli(["run", current.job.id, "--manual"]);
      setMode({ kind: "log", id: current.job.id });
    } else if (k === "p" && current) deps.spawnCli([current.held ? "resume" : "hold", current.job.id]);
    else if (k === "o" && current?.job.url) open(current.job.url);
    else if (k === "s") {
      try {
        setMode({ kind: "sync", plan: planSync(loadConfig(), defaultCtx()) });
      } catch (e) {
        const label = e instanceof ConfigError ? "CONFIG ERROR" : "ERROR";
        setMode({ kind: "sync", plan: null, error: `${label} — ${(e as Error).message}` });
      }
    }
  });

  if (mode.kind === "log") return <LogView id={mode.id} follow />;
  if (mode.kind === "help") return <HelpView view={view} />;
  if (view === "services") {
    const p: Pending = new Map([...pending].map(([k, v]) => [k, v.kind]));
    const prompt =
      mode.kind === "stack"
        ? `${mode.start ? "start" : "stop"} ${mode.view.services.filter((v) => isUp(v) !== mode.start).length} services in ${mode.view.stack.name.toUpperCase()}?  Y confirm · any key cancel`
        : null;
    return (
      <ServicesView stacks={stackViews} error={actionError ?? svcError} selected={svcIndex} now={now} pending={p} prompt={prompt} />
    );
  }
  const svcTotals = stackViews?.length ? totals(services) : null;
  const dockerLine = svcTotals
    ? `${svcTotals.up}/${svcTotals.all} up · cpu ${svcTotals.cpu.toFixed(1)}% · mem ${formatBytes(svcTotals.mem)}`
    : null;
  const banner = loadError ? <Text color={color.fault}>{`  ERROR — ${loadError}`}</Text> : null;
  if (!snap)
    return (
      <Box flexDirection="column">
        <Text color={color.body}>   ◢ H Q  ·  linking…</Text>
        {banner}
      </Box>
    );
  return (
    <Box flexDirection="column">
      {banner}
      <Dashboard snapshot={snap} selected={Math.min(selected, Math.max(0, jobs.length - 1))} now={now} services={dockerLine} />
      {mode.kind === "sync" ? <SyncPrompt mode={mode} /> : null}
    </Box>
  );
}

function SyncPrompt({ mode }: { mode: Extract<Mode, { kind: "sync" }> }) {
  const { plan, error } = mode;
  const empty = plan && !plan.add.length && !plan.change.length && !plan.remove.length;
  return (
    <Box flexDirection="column" marginLeft={2}>
      {error ? <Text color={color.fault}>{error}</Text> : null}
      {plan?.add.map((id) => <Text key={`a${id}`} color={color.accent}>{`+ ${id}`}</Text>)}
      {plan?.change.map((id) => <Text key={`c${id}`} color={color.body}>{`~ ${id}`}</Text>)}
      {plan?.remove.map((id) => <Text key={`r${id}`} color={color.fault}>{`- ${id}`}</Text>)}
      <Text color={color.hold}>{empty ? "in sync · any key to close" : plan ? "apply sync?  Y confirm · any key cancel" : "any key to close"}</Text>
    </Box>
  );
}
