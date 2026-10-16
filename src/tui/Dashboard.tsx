import { Box, Text, useStdout } from "ink";
import { type Snapshot, summarize } from "../snapshot";
import { DetailPanel } from "./DetailPanel";
import { JobRow } from "./JobRow";
import { clockLine, color, innerWidth, MARGIN } from "./theme";

export const KEYS = "↑↓ select   ⏎ logs   R run   P hold   S sync   H help   Q exit";

export const Rule = ({ width }: { width: number }) => (
  <Box marginLeft={MARGIN}>
    <Text color={color.hold}>{"─".repeat(width)}</Text>
  </Box>
);

const n2 = (n: number) => String(n).padStart(2, "0");

export function Dashboard({ snapshot, selected, now }: { snapshot: Snapshot; selected: number; now: Date }) {
  const { stdout } = useStdout();
  const width = innerWidth(stdout.columns);
  const sum = summarize(snapshot);
  const current = snapshot.jobs[selected];
  return (
    <Box flexDirection="column" paddingTop={1}>
      <Box marginLeft={MARGIN} width={width} justifyContent="space-between">
        <Text color={color.accent} bold>◢ H Q</Text>
        <Text color={color.body}>{clockLine(now)}</Text>
      </Box>
      {snapshot.configError ? (
        <Box marginLeft={MARGIN} width={width}>
          <Text color={color.fault} wrap="truncate-end">{`CONFIG ERROR — ${snapshot.configError}`}</Text>
        </Box>
      ) : null}
      <Rule width={width} />
      <Box marginLeft={MARGIN} width={width} justifyContent="space-between">
        <Text color={color.body}>{`SYSTEMS ${n2(sum.systems)}`}</Text>
        <Text color={color.body}>{`NOMINAL ${n2(sum.nominal)}`}</Text>
        <Text color={sum.fault ? color.fault : color.body}>{`FAULT ${n2(sum.fault)}`}</Text>
        <Text color={color.body}>{`STANDBY ${n2(sum.standby)}`}</Text>
      </Box>
      <Rule width={width} />
      {snapshot.jobs.map((v, i) => (
        <JobRow key={v.job.id} view={v} selected={i === selected} now={now} width={width} />
      ))}
      {current ? (
        <>
          <Rule width={width} />
          <DetailPanel view={current} width={width} />
        </>
      ) : (
        <Box marginLeft={MARGIN}>
          <Text color={color.hold}>no jobs in jobs.toml</Text>
        </Box>
      )}
      <Rule width={width} />
      <Box marginLeft={MARGIN} width={width}>
        <Text color={color.hold} wrap="truncate-end">{KEYS}</Text>
      </Box>
    </Box>
  );
}
