import { Box, Text, useStdout } from "ink";
import { Rule } from "./Dashboard";
import { color, innerWidth, MARGIN } from "./theme";

type Row = [symbol: string, meaning: string, tone?: string];

const SECTIONS: [title: string, rows: Row[]][] = [
  ["JOB STATUS", [
    ["◉", "nominal — the last run succeeded", color.accent],
    ["◆", "fault — the last run failed (EXIT n) or timed out", color.fault],
    ["○", "hold — paused, no scheduled runs", color.hold],
    ["⚠", "not synced — launchd differs from jobs.toml; press S", color.fault],
    ["RUNNING", "a run is in progress"],
    ["T–hh:mm:ss", "countdown to the next scheduled run"],
    ["ON DEMAND · R", "manual = true: never scheduled, R runs it"],
  ]],
  ["SUMMARY", [
    ["SYSTEMS", "jobs in jobs.toml"],
    ["NOMINAL", "last run ok"],
    ["FAULT", "last run failed or timed out"],
    ["STANDBY", "held, or never run"],
  ]],
  ["HISTORY", [["▁  █  ·", "last 10 runs, newest right: ok · fault · skipped"]]],
  ["DETAILS", [
    ["cmd · dir", "command (run with zsh -lc) and working directory; url: O opens it"],
    ["quiet", "scheduled runs are skipped in this window (R still runs)"],
    ["timeout", "SIGTERM after this long, SIGKILL 10 s later"],
    ["launchd", "linked = loaded · held · unlinked = not loaded"],
    ["›", "last line of output from the latest run"],
  ]],
];

const SERVICE_SECTIONS: [title: string, rows: Row[]][] = [
  ["SERVICES", [
    ["◉  ○  ◆", "running · stopped or not created · unhealthy, restarting or crashed", color.accent],
    ["CPU  MEM", "live usage; CPU is % of one core (can pass 100%); amber over 50% or 1 GiB"],
    ["O · X · Z", "open localhost:<port> · stop/start one service · stop/start the stack"],
    ["[stacks.x]", "dir = compose project dir; optional files = [...], env_file"],
  ]],
];

export function HelpView({ view = "jobs" }: { view?: "jobs" | "services" }) {
  const sections = view === "services" ? SERVICE_SECTIONS : SECTIONS;
  const { stdout } = useStdout();
  const width = innerWidth(stdout.columns);
  const col = 12;
  return (
    <Box flexDirection="column" paddingTop={1}>
      <Box marginLeft={MARGIN} width={width} justifyContent="space-between">
        <Text color={color.accent} bold>◢ H Q · HELP</Text>
        <Text color={color.hold}>esc / H close</Text>
      </Box>
      <Rule width={width} />
      {sections.map(([title, rows]) => (
        <Box key={title} flexDirection="column" marginLeft={MARGIN} width={width}>
          <Text color={color.hold}>{title}</Text>
          {rows.map(([sym, meaning, tone]) => (
            <Text key={sym} wrap="truncate-end">
              <Text color={tone ?? color.accent}>{`  ${sym.padEnd(col)}`}</Text>
              <Text color={color.body}>{meaning}</Text>
            </Text>
          ))}
        </Box>
      ))}
      <Rule width={width} />
    </Box>
  );
}
