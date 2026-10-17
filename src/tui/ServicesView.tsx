import { Box, Text, useStdout } from "ink";
import { formatBytes, isServiceFault, isUp, type ServiceView, serviceUrl, type StackView, totals } from "../stacks";
import { Rule } from "./Dashboard";
import { clockLine, color, INDENT, innerWidth, MARGIN } from "./theme";

export const SERVICE_KEYS = "↑↓ select   O open   X stop/start   Z whole stack   Tab jobs   H help   Q exit";

export type Pending = ReadonlyMap<string, "starting" | "stopping">;
export const serviceKey = (v: Pick<ServiceView, "stack" | "service">) => `${v.stack}/${v.service}`;

export function serviceGlyph(v: ServiceView): "◉" | "◆" | "○" {
  if (isServiceFault(v)) return "◆";
  return isUp(v) ? "◉" : "○";
}

function stateText(v: ServiceView, pending: Pending): string {
  const p = pending.get(serviceKey(v));
  if (p) return `${p}…`;
  if (v.state === "missing") return "not created";
  if (v.state === "running") {
    const up = `up ${shortUptime(v.status)}`;
    return v.health && v.health !== "healthy" ? `${v.health} · ${up}` : up;
  }
  if (v.state === "exited") return isServiceFault(v) ? `exit ${v.exitCode}` : "stopped";
  return v.state;
}

/** Docker's "Up 39 hours (healthy)" / "Up About an hour" → "39h" / "1h". */
export function shortUptime(status: string): string {
  const s = status.replace(/^Up\s+/i, "").replace(/\s*\(.*\)$/, "").trim();
  if (/^less than a second$/i.test(s)) return "<1s";
  const m = /^(\d+|about an?|an?)\s+(second|minute|hour|day|week|month|year)s?$/i.exec(s);
  if (!m) return s;
  const n = /^\d+$/.test(m[1]!) ? m[1] : "1";
  const unit = { second: "s", minute: "m", hour: "h", day: "d", week: "w", month: "mo", year: "y" }[m[2]!.toLowerCase()]!;
  return `${n}${unit}`;
}

const pct = (n: number | null) => (n === null ? "" : `${n.toFixed(1)}%`);
const mem = (n: number | null) => (n === null ? "" : formatBytes(n));

/** Heavier than this is drawn in amber so the culprits stand out. */
const HEAVY = { cpu: 50, mem: 1024 ** 3 };

function ServiceRow({ v, selected, pending }: { v: ServiceView; selected: boolean; pending: Pending }) {
  const g = serviceGlyph(v);
  const gColor = g === "◆" ? color.fault : g === "○" ? color.hold : selected ? color.accent : color.body;
  const nameColor = selected ? color.accent : isUp(v) ? color.body : color.hold;
  const heavy = (v.cpu ?? 0) >= HEAVY.cpu || (v.mem ?? 0) >= HEAVY.mem;
  return (
    <Text wrap="truncate-end">
      <Text color={gColor}>{g}</Text>
      <Text color={nameColor} bold={selected}>{`  ${v.service.toUpperCase().padEnd(26)}`}</Text>
      <Text color={g === "◆" ? color.fault : color.body} dimColor={!isUp(v)}>{stateText(v, pending).padEnd(18)}</Text>
      <Text color={heavy ? color.fault : color.body}>{pct(v.cpu).padStart(7)}{mem(v.mem).padStart(8)}</Text>
      <Text color={color.hold}>{(v.ports[0] ? `  :${v.ports[0]}` : "").padEnd(8)}</Text>
    </Text>
  );
}

const field = (label: string, value: string) => (
  <Text wrap="truncate-end">
    <Text color={color.hold}>{label.padEnd(10)}</Text>
    <Text color={color.body}>{value}</Text>
  </Text>
);

/** Rows above and below the list: title, rules, summary, detail panel, keys. */
const CHROME = 15;

export function ServicesView(props: {
  stacks: StackView[] | null;
  error: string | null;
  selected: number;
  now: Date;
  pending: Pending;
  prompt: string | null;
}) {
  const { stdout } = useStdout();
  const width = innerWidth(stdout.columns);
  const stacks = props.stacks ?? [];
  const all = stacks.flatMap((s) => s.services);
  const sum = totals(all);
  const current = all[props.selected];

  // One flat list of stack headers and service rows, windowed around the selection.
  type Item = { kind: "stack"; s: StackView } | { kind: "svc"; v: ServiceView; index: number };
  const items: Item[] = [];
  let index = 0;
  for (const s of stacks) {
    items.push({ kind: "stack", s });
    for (const v of s.services) items.push({ kind: "svc", v, index: index++ });
  }
  const room = Math.max(4, (stdout.rows || 40) - CHROME);
  const at = items.findIndex((i) => i.kind === "svc" && i.index === props.selected);
  const first = Math.max(0, Math.min(at - Math.floor(room / 2), items.length - room));
  const shown = items.slice(first, first + room);

  const url = current ? serviceUrl(current) : null;
  return (
    <Box flexDirection="column" paddingTop={1}>
      <Box marginLeft={MARGIN} width={width} justifyContent="space-between">
        <Text color={color.accent} bold>◢ H Q · SERVICES</Text>
        <Text color={color.body}>{clockLine(props.now)}</Text>
      </Box>
      {props.error ? (
        <Box marginLeft={MARGIN} width={width}>
          <Text color={color.fault} wrap="truncate-end">{`ERROR — ${props.error}`}</Text>
        </Box>
      ) : null}
      <Rule width={width} />
      <Box marginLeft={MARGIN} width={width} justifyContent="space-between">
        <Text color={color.body}>{`UP ${sum.up}/${sum.all}`}</Text>
        <Text color={color.body}>{`CPU ${sum.cpu.toFixed(1)}%`}</Text>
        <Text color={color.body}>{`MEM ${formatBytes(sum.mem)}`}</Text>
        <Text color={color.hold}>{`STACKS ${stacks.length}`}</Text>
      </Box>
      <Rule width={width} />
      <Box flexDirection="column" marginLeft={MARGIN} width={width}>
        {props.stacks === null ? <Text color={color.hold}>reading docker…</Text> : null}
        {props.stacks !== null && stacks.length === 0 ? (
          <Text color={color.hold}>no stacks — add [stacks.&lt;name&gt;] dir = "…" to jobs.toml</Text>
        ) : null}
        {shown.map((item) => {
          if (item.kind === "svc")
            return <ServiceRow key={serviceKey(item.v)} v={item.v} selected={item.index === props.selected} pending={props.pending} />;
          const t = totals(item.s.services);
          const label = `${item.s.stack.name.toUpperCase()} · ${t.up}/${t.all} up`;
          const right = item.s.error ? item.s.error : t.up ? `${t.cpu.toFixed(1)}%  ${formatBytes(t.mem)}` : "";
          return (
            <Box key={`#${item.s.stack.name}`} justifyContent="space-between">
              <Text color={color.hold}>{label}</Text>
              <Text color={item.s.error ? color.fault : color.hold} wrap="truncate-end">{right}</Text>
            </Box>
          );
        })}
      </Box>
      {current ? (
        <>
          <Rule width={width} />
          <Box flexDirection="column" marginLeft={MARGIN} width={width}>
            <Text wrap="truncate-end">
              <Text color={color.accent}>{`▸  ${current.service.toUpperCase()}`}</Text>
              <Text color={color.body}>{` — ${current.image}`}</Text>
            </Text>
            <Box flexDirection="column" marginLeft={INDENT} width={width - INDENT}>
              {field("url", url ?? "— (no published port)")}
              {field("status", `${current.status}${current.container ? ` · ${current.container}` : ""}`)}
            </Box>
          </Box>
        </>
      ) : null}
      <Rule width={width} />
      <Box marginLeft={MARGIN} width={width}>
        {props.prompt ? (
          <Text color={color.fault} wrap="truncate-end">{props.prompt}</Text>
        ) : (
          <Text color={color.hold} wrap="truncate-end">{SERVICE_KEYS}</Text>
        )}
      </Box>
    </Box>
  );
}
