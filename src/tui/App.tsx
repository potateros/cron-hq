import { Box, Text, useApp, useInput } from "ink";
import { useEffect, useRef, useState } from "react";
import { ConfigError, loadConfig } from "../config";
import type { History } from "../history";
import { defaultCtx, type Exec, planSync, type SyncPlan } from "../launchd";
import { loadSnapshot, type Snapshot } from "../snapshot";
import { Dashboard } from "./Dashboard";
import { HelpView } from "./HelpView";
import { LogView } from "./LogView";
import { color } from "./theme";

export interface AppDeps {
  exec: Exec;
  history: History;
  spawnCli: (args: string[]) => void;
  clock?: () => Date;
  onExit?: () => void;
}

type Mode = { kind: "dash" } | { kind: "help" } | { kind: "log"; id: string } | { kind: "sync"; plan: SyncPlan | null; error?: string };

export function App({ deps }: { deps: AppDeps }) {
  const clock = deps.clock ?? (() => new Date());
  const { exit } = useApp();
  const [now, setNow] = useState(clock);
  const [snap, setSnap] = useState<Snapshot | null>(null);
  const [selected, setSelected] = useState(0);
  const [mode, setMode] = useState<Mode>({ kind: "dash" });
  const prev = useRef<Snapshot | undefined>(undefined);
  const [loadError, setLoadError] = useState<string | null>(null);

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

  const jobs = snap?.jobs ?? [];
  const current = jobs[Math.min(selected, jobs.length - 1)];

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
    if (key.upArrow) setSelected((i) => Math.max(0, i - 1));
    else if (key.downArrow) setSelected((i) => Math.min(jobs.length - 1, i + 1));
    else if (key.return && current) setMode({ kind: "log", id: current.job.id });
    else if (k === "r" && current) {
      deps.spawnCli(["run", current.job.id, "--manual"]);
      setMode({ kind: "log", id: current.job.id });
    } else if (k === "p" && current) deps.spawnCli([current.held ? "resume" : "hold", current.job.id]);
    else if (k === "h") setMode({ kind: "help" });
    else if (k === "s") {
      try {
        setMode({ kind: "sync", plan: planSync(loadConfig(), defaultCtx()) });
      } catch (e) {
        const label = e instanceof ConfigError ? "CONFIG ERROR" : "ERROR";
        setMode({ kind: "sync", plan: null, error: `${label} — ${(e as Error).message}` });
      }
    } else if (k === "q") {
      deps.onExit?.();
      exit();
    }
  });

  if (mode.kind === "log") return <LogView id={mode.id} follow />;
  if (mode.kind === "help") return <HelpView />;
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
      <Dashboard snapshot={snap} selected={Math.min(selected, Math.max(0, jobs.length - 1))} now={now} />
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
