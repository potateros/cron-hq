import { Box, Text } from "ink";
import type { JobView } from "../snapshot";
import { formatDuration } from "../runner";
import { describeSchedule, formatCountdown } from "../schedule";
import { color, glyph, hhmm, isFault } from "./theme";

function historyStrip(v: JobView): { mark: string; c: string }[] {
  return [...v.recent].reverse().slice(-10).map((r) =>
    r.status === "skipped"
      ? { mark: "·", c: color.hold }
      : r.status === "failed" || r.status === "timeout"
        ? { mark: "█", c: color.fault }
        : { mark: "▁", c: color.body },
  );
}

function rightStatus(v: JobView, now: Date): { text: string; c: string } {
  if (v.held) return { text: "HOLD", c: color.hold };
  if (v.running) return { text: "RUNNING", c: color.accent };
  const countdown = v.next ? formatCountdown(v.next.getTime() - now.getTime()) : "";
  if (!v.synced) return { text: `⚠ NOT SYNCED   ${countdown}`.trimEnd(), c: color.fault };
  if (isFault(v)) {
    const why = v.last!.status === "timeout" ? "TIMEOUT" : `EXIT ${v.last!.exitCode}`;
    return { text: `FAULT · ${why}   ${countdown}`.trimEnd(), c: color.fault };
  }
  return { text: countdown, c: color.body };
}

function detailLine(v: JobView): string {
  const parts = [describeSchedule(v.job.schedule)];
  if (v.held) parts.push("paused");
  else if (!v.last) parts.push("never run");
  else {
    parts.push(`last ${hhmm(v.last.startedAt)}`);
    if (v.last.endedAt) parts.push(formatDuration(v.last.endedAt - v.last.startedAt));
  }
  return parts.join("  ·  ");
}

export function JobRow({ view, selected, now, width }: { view: JobView; selected: boolean; now: Date; width: number }) {
  const g = glyph(view);
  const gColor = g === "◆" || g === "⚠" ? color.fault : g === "○" ? color.hold : selected ? color.accent : color.body;
  const status = rightStatus(view, now);
  const strip = historyStrip(view);
  return (
    <Box flexDirection="column" marginLeft={5} width={width - 5} marginBottom={1}>
      <Box justifyContent="space-between">
        <Text>
          <Text color={gColor}>{g}</Text>
          <Text color={selected ? color.accent : color.body} bold={selected}>{`  ${view.job.id.toUpperCase()}`}</Text>
        </Text>
        <Text color={status.c}>{status.text}</Text>
      </Box>
      <Box justifyContent="space-between">
        <Text color={color.body} dimColor={!selected}>{`   ${detailLine(view)}`}</Text>
        <Text>
          {strip.map((s, i) => (
            <Text key={i} color={s.c}>{`${i ? " " : ""}${s.mark}`}</Text>
          ))}
        </Text>
      </Box>
    </Box>
  );
}
