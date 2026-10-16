import { Box, Text } from "ink";
import type { JobView } from "../snapshot";
import { clampLines, color, formatSeconds, INDENT, MARGIN, minutesToClock } from "./theme";

const LABEL = 10;

const field = (label: string, value: string) => (
  <Text>
    <Text color={color.hold}>{label.padEnd(LABEL)}</Text>
    <Text color={color.body}>{value}</Text>
  </Text>
);

export function DetailPanel({ view, width }: { view: JobView; width: number }) {
  const { job } = view;
  const quiet = job.quiet ? `${minutesToClock(job.quiet.start)} – ${minutesToClock(job.quiet.end)}` : "—";
  const launchd = view.held ? "held" : view.agent.linked ? "linked" : "unlinked";
  const lastLine = view.last?.tail?.split("\n").filter((l) => l.trim()).at(-1) ?? "no output yet";
  const inner = width - INDENT;
  const output = clampLines(lastLine, inner - 2, 2);
  return (
    <Box flexDirection="column" marginLeft={MARGIN} width={width}>
      <Text wrap="truncate-end">
        <Text color={color.accent}>{`▸  ${job.id.toUpperCase()}`}</Text>
        {job.description ? <Text color={color.body}>{` — ${job.description}`}</Text> : null}
      </Text>
      <Box flexDirection="column" marginLeft={INDENT} width={inner}>
        <Text wrap="truncate-end">{field("cmd", job.command)}</Text>
        <Text wrap="truncate-end">{field("dir", job.cwd)}</Text>
        <Text wrap="truncate-end">
          {field("quiet", quiet.padEnd(18))}
          {field("timeout", formatSeconds(job.timeoutSec).padEnd(8))}
          {field("launchd", launchd)}
        </Text>
        {output.map((l, i) => (
          <Text key={i} color={color.body}>{`${i ? "  " : "› "}${l}`}</Text>
        ))}
      </Box>
    </Box>
  );
}
