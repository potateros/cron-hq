import { Box, Text, useStdout } from "ink";
import { type Snapshot, summarize } from "../snapshot";
import { DetailPanel } from "./DetailPanel";
import { JobRow } from "./JobRow";
import { clockLine, color, MAX_WIDTH } from "./theme";

export const KEYS = "↑↓ select     ⏎ logs     R run     P hold     S sync     Q exit";

const Divider = ({ width }: { width: number }) => (
  <Box marginLeft={3} marginY={1}>
    <Text color={color.hold}>{"─".repeat(Math.max(10, width - 6))}</Text>
  </Box>
);

const n2 = (n: number) => String(n).padStart(2, "0");

export function Dashboard({ snapshot, selected, now }: { snapshot: Snapshot; selected: number; now: Date }) {
  const { stdout } = useStdout();
  const width = Math.min(stdout.columns || MAX_WIDTH, MAX_WIDTH);
  const sum = summarize(snapshot);
  const current = snapshot.jobs[selected];
  return (
    <Box flexDirection="column" width={width} paddingTop={1}>
      <Box marginLeft={3} width={width - 6} justifyContent="space-between">
        <Text color={color.accent} bold>◢ H Q</Text>
        <Text color={color.body}>{clockLine(now)}</Text>
      </Box>
      {snapshot.configError ? (
        <Box marginLeft={3} marginTop={1}>
          <Text color={color.fault}>{`CONFIG ERROR — ${snapshot.configError}`}</Text>
        </Box>
      ) : null}
      <Divider width={width} />
      <Box marginLeft={5} marginBottom={2}>
        <Text color={color.body}>
          {`SYSTEMS  ${n2(sum.systems)}          NOMINAL  ${n2(sum.nominal)}          `}
          <Text color={sum.fault ? color.fault : color.body}>{`FAULT  ${n2(sum.fault)}`}</Text>
          {`          STANDBY  ${n2(sum.standby)}`}
        </Text>
      </Box>
      {snapshot.jobs.map((v, i) => (
        <JobRow key={v.job.id} view={v} selected={i === selected} now={now} width={width - 6} />
      ))}
      {current ? (
        <>
          <Divider width={width} />
          <DetailPanel view={current} />
        </>
      ) : (
        <Box marginLeft={5}>
          <Text color={color.hold}>no jobs in jobs.toml</Text>
        </Box>
      )}
      <Divider width={width} />
      <Box marginLeft={5}>
        <Text color={color.hold}>{KEYS}</Text>
      </Box>
    </Box>
  );
}
