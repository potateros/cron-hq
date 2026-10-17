import { existsSync, readFileSync, watch } from "node:fs";
import { Box, Text, useInput, useStdout } from "ink";
import { useEffect, useRef, useState } from "react";
import { logPath } from "../paths";
import { Rule } from "./Dashboard";
import { color, innerWidth, MARGIN } from "./theme";

const read = (id: string) => {
  if (!existsSync(logPath(id))) return [];
  const lines = readFileSync(logPath(id), "utf8").split("\n");
  if (lines.at(-1) === "") lines.pop(); // the file ends with a newline
  return lines;
};

function lineColor(line: string): string {
  if (line.startsWith("── RUN")) return color.accent;
  if (line.startsWith("── END")) return line.includes("NOMINAL") ? color.accent : color.fault;
  return color.body;
}

/** Width of the "▸ " cursor gutter in front of every line. */
const GUTTER = 2;

/** Hard-wrap `line` into rows of at most `width` characters. Always at least one row. */
export function chunk(line: string, width: number): string[] {
  const chars = Array.from(line);
  if (chars.length <= width) return [line];
  const rows: string[] = [];
  for (let i = 0; i < chars.length; i += width) rows.push(chars.slice(i, i + width).join(""));
  return rows;
}

/** One line as it is drawn: the full wrap when expanded, otherwise one row cut with "…". */
export function rowsFor(line: string, width: number, expanded: boolean): string[] {
  const rows = chunk(line, width);
  if (expanded || rows.length === 1) return rows;
  return [`${Array.from(line).slice(0, width - 1).join("")}…`];
}

/**
 * First line of the viewport, so that `cursor` is on screen.
 * `anchor: "bottom"` packs as many lines as fit above the cursor (following the tail);
 * otherwise it moves `start` as little as possible from where it was.
 */
export function viewTop(
  heights: number[],
  cursor: number,
  height: number,
  start: number,
  anchor: "bottom" | "keep",
): number {
  if (heights.length === 0) return 0;
  if (anchor === "bottom") {
    let top = cursor;
    let used = heights[cursor]!;
    while (top > 0 && used + heights[top - 1]! <= height) used += heights[--top]!;
    return top;
  }
  let top = Math.min(Math.max(0, start), cursor);
  let used = 0;
  for (let i = top; i <= cursor; i++) used += heights[i]!;
  while (top < cursor && used > height) used -= heights[top++]!;
  return top;
}

/**
 * Full-screen log tail with a line cursor. Following (the default) keeps the cursor on the
 * newest line; moving up detaches it. ⏎ expands the cursor line, `w` wraps every line.
 */
export function LogView({ id, follow }: { id: string; follow: boolean }) {
  const { stdout } = useStdout();
  const height = Math.max(5, (stdout.rows || 24) - 5);
  const width = innerWidth(stdout.columns);
  const textWidth = Math.max(10, width - GUTTER);
  const [lines, setLines] = useState(() => read(id));
  const [cursor, setCursor] = useState<number | null>(null); // null = following the tail
  const [expanded, setExpanded] = useState<ReadonlySet<number>>(new Set());
  const [wrapAll, setWrapAll] = useState(false);
  const start = useRef(0);

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

  // The log rotated (or was truncated): line numbers no longer mean the same lines.
  const last = lines.length - 1;
  useEffect(() => {
    if (cursor !== null && cursor > last) {
      setCursor(null);
      setExpanded(new Set());
    }
  }, [cursor, last]);

  const at = cursor === null ? last : Math.min(cursor, last);
  const rows = lines.map((l, i) => rowsFor(l, textWidth, wrapAll || expanded.has(i)));
  const heights = rows.map((r) => r.length);
  const top = viewTop(heights, Math.max(0, at), height, start.current, cursor === null ? "bottom" : "keep");
  start.current = top;

  const moveTo = (i: number) => setCursor(i >= last ? null : Math.max(0, i));
  useInput((input, key) => {
    if (lines.length === 0) return;
    if (key.upArrow) moveTo(at - 1);
    if (key.downArrow) moveTo(at + 1);
    if (key.pageUp) moveTo(at - height);
    if (key.pageDown) moveTo(at + height);
    if (input === "g") {
      start.current = 0;
      moveTo(0);
    }
    if (input === "G") setCursor(null);
    if (key.return || input === " ") {
      setExpanded((s) => {
        const next = new Set(s);
        if (!next.delete(at)) next.add(at);
        return next;
      });
    }
    if (input === "w") setWrapAll((w) => !w);
  });

  const visible: { text: string; line: number; first: boolean }[] = [];
  for (let i = top; i < rows.length && visible.length < height; i++) {
    for (const [j, text] of rows[i]!.entries()) {
      if (visible.length >= height) break;
      visible.push({ text, line: i, first: j === 0 });
    }
  }

  const status = [cursor === null ? "LIVE" : `${at + 1}/${lines.length}`, wrapAll ? "WRAP" : ""].filter(Boolean);
  return (
    <Box flexDirection="column" paddingTop={1}>
      <Box marginLeft={MARGIN} width={width} justifyContent="space-between">
        <Text color={color.accent} bold>{`◢ H Q · LOG · ${id.toUpperCase()}`}</Text>
        <Text color={color.hold}>{status.join(" · ")}</Text>
      </Box>
      <Rule width={width} />
      <Box flexDirection="column" marginLeft={MARGIN} width={width} height={height}>
        {visible.length ? (
          visible.map((r, i) => {
            const mark = r.line === at && r.first ? "▸ " : "  ";
            return (
              <Text key={i} wrap="truncate-end">
                <Text color={color.accent}>{mark}</Text>
                <Text color={lineColor(lines[r.line]!)} bold={r.line === at}>{r.text || " "}</Text>
              </Text>
            );
          })
        ) : (
          <Text color={color.hold}>no log yet</Text>
        )}
      </Box>
      <Rule width={width} />
      <Box marginLeft={MARGIN}>
        <Text color={color.hold}>↑↓ line   PgUp PgDn page   ⏎ expand   w wrap all   g G top/live   esc back</Text>
      </Box>
    </Box>
  );
}
