import { Box, Text } from "ink";
import type { JobView } from "../snapshot";
import { color, formatSeconds, minutesToClock } from "./theme";

const field = (label: string, value: string) => (
  <Text>
    <Text color={color.hold}>{label.padEnd(10)}</Text>
    <Text color={color.body}>{value}</Text>
  </Text>
);

export function DetailPanel({ view }: { view: JobView }) {
  const { job } = view;
  const quiet = job.quiet ? `${minutesToClock(job.quiet.start)} – ${minutesToClock(job.quiet.end)}` : "—";
  const launchd = view.held ? "held" : view.agent.linked ? "linked" : "unlinked";
  const lastLine = view.last?.tail?.split("\n").filter((l) => l.trim()).at(-1) ?? "no output yet";
  return (
    <Box flexDirection="column" marginLeft={5}>
      <Text>
        <Text color={color.accent}>{`▸  ${job.id.toUpperCase()}`}</Text>
        {job.description ? <Text color={color.body}>{`  —  ${job.description}`}</Text> : null}
      </Text>
      <Box flexDirection="column" marginLeft={3} marginTop={1}>
        {field("cmd", job.command)}
        {field("dir", job.cwd)}
        <Text>
          {field("quiet", quiet.padEnd(23))}
          {field("timeout", formatSeconds(job.timeoutSec).padEnd(12))}
          {field("launchd", launchd)}
        </Text>
        <Box marginTop={1}>
          <Text color={color.body}>{`› ${lastLine}`}</Text>
        </Box>
      </Box>
    </Box>
  );
}
