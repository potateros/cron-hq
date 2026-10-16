import { existsSync, readFileSync, watch } from "node:fs";
import { Box, Text, useInput, useStdout } from "ink";
import { useEffect, useState } from "react";
import { logPath } from "../paths";
import { Rule } from "./Dashboard";
import { color, innerWidth, MARGIN } from "./theme";

const read = (id: string) => (existsSync(logPath(id)) ? readFileSync(logPath(id), "utf8").split("\n") : []);

function lineColor(line: string): string {
  if (line.startsWith("── RUN")) return color.accent;
  if (line.startsWith("── END")) return line.includes("NOMINAL") ? color.accent : color.fault;
  return color.body;
}

/** Full-screen log tail. `offset` counts lines up from the bottom; 0 = following the tail. */
export function LogView({ id, follow }: { id: string; follow: boolean }) {
  const { stdout } = useStdout();
  const height = Math.max(5, (stdout.rows || 24) - 5);
  const width = innerWidth(stdout.columns);
  const [lines, setLines] = useState(() => read(id));
  const [offset, setOffset] = useState(0);

  useEffect(() => {
    if (!follow) return;
    const path = logPath(id);
    const refresh = () => setLines(read(id));
    const timer = setInterval(refresh, 1000); // also catches the file appearing / rotating
    let watcher: ReturnType<typeof watch> | undefined;
    try {
      watcher = existsSync(path) ? watch(path, refresh) : undefined;
    } catch {}
    return () => {
      clearInterval(timer);
      watcher?.close();
    };
  }, [id, follow]);

  const maxOffset = Math.max(0, lines.length - height);
  useInput((input, key) => {
    if (key.upArrow) setOffset((o) => Math.min(maxOffset, o + 1));
    if (key.downArrow) setOffset((o) => Math.max(0, o - 1));
    if (key.pageUp) setOffset((o) => Math.min(maxOffset, o + height));
    if (key.pageDown) setOffset((o) => Math.max(0, o - height));
    if (input === "g") setOffset(maxOffset);
    if (input === "G") setOffset(0);
  });

  const end = lines.length - Math.min(offset, maxOffset);
  const visible = lines.slice(Math.max(0, end - height), end);
  return (
    <Box flexDirection="column" paddingTop={1}>
      <Box marginLeft={MARGIN} width={width} justifyContent="space-between">
        <Text color={color.accent} bold>{`◢ H Q · LOG · ${id.toUpperCase()}`}</Text>
        <Text color={color.hold}>{offset === 0 ? "LIVE" : `−${offset}`}</Text>
      </Box>
      <Rule width={width} />
      <Box flexDirection="column" marginLeft={MARGIN} width={width} height={height}>
        {visible.length ? (
          visible.map((l, i) => (
            <Text key={i} color={lineColor(l)} wrap="truncate-end">{l || " "}</Text>
          ))
        ) : (
          <Text color={color.hold}>no log yet</Text>
        )}
      </Box>
      <Rule width={width} />
      <Box marginLeft={MARGIN}>
        <Text color={color.hold}>↑↓ scroll   PgUp PgDn page   g G top/bottom   esc back</Text>
      </Box>
    </Box>
  );
}
