#!/usr/bin/env bun
/**
 * Regenerate docs/images/*.png: renders the real TUI components with demo data, converts the
 * ANSI output to HTML and screenshots it with headless Chrome.
 *
 *   bun tools/screenshots.tsx
 */
import { mkdirSync, mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

process.env.FORCE_COLOR = "3";
process.env.HQ_HOME = mkdtempSync(join(tmpdir(), "hq-shots-"));

const { render } = await import("ink-testing-library");
const { Dashboard } = await import("../src/tui/Dashboard");
const { HelpView } = await import("../src/tui/HelpView");
const { LogView } = await import("../src/tui/LogView");
const { ServicesView } = await import("../src/tui/ServicesView");
const { appendLog } = await import("../src/log");
const { demoLog, demoStacks, NOW, snapshot } = await import("./demo");

const CHROME = "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome";
const OUT = resolve(import.meta.dir, "..", "docs", "images");
const BG = "#0b1216";
const FONT_PX = 14;
const CHAR_W = 8.43; // Menlo advance at 14 px
const LINE_H = 18;
const PAD = 24;

const esc = (s: string) => s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");

/** Minimal SGR → HTML: truecolor fg, bold, dim, resets. Enough for what Ink emits. */
function ansiToHtml(text: string): string {
  let fg: string | null = null;
  let bold = false;
  let dim = false;
  let html = "";
  for (const part of text.split(/(\x1b\[[0-9;]*m)/)) {
    const m = /^\x1b\[([0-9;]*)m$/.exec(part);
    if (!m) {
      if (!part) continue;
      const style = [fg && `color:${fg}`, bold && "font-weight:bold", dim && "opacity:.6"].filter(Boolean).join(";");
      html += style ? `<span style="${style}">${esc(part)}</span>` : esc(part);
      continue;
    }
    const codes = (m[1] || "0").split(";").map(Number);
    for (let i = 0; i < codes.length; i++) {
      const c = codes[i]!;
      if (c === 0) [fg, bold, dim] = [null, false, false];
      else if (c === 1) bold = true;
      else if (c === 2) dim = true;
      else if (c === 22) [bold, dim] = [false, false];
      else if (c === 39) fg = null;
      else if (c === 38 && codes[i + 1] === 2) {
        fg = `rgb(${codes[i + 2]},${codes[i + 3]},${codes[i + 4]})`;
        i += 4;
      }
    }
  }
  return html;
}

async function shoot(name: string, el: React.ReactElement) {
  const r = render(el);
  await Bun.sleep(100);
  const frame = r.lastFrame()!;
  r.unmount();
  const lines = frame.split("\n");
  const cols = Math.max(...lines.map((l) => [...l.replace(/\x1b\[[0-9;]*m/g, "")].length));
  const width = Math.ceil(cols * CHAR_W) + 2 * PAD;
  const height = lines.length * LINE_H + 2 * PAD;
  const page = join(process.env.HQ_HOME!, `${name}.html`);
  writeFileSync(
    page,
    `<!doctype html><meta charset="utf-8"><style>
      html,body{margin:0;background:${BG}}
      pre{margin:0;padding:${PAD}px;font:${FONT_PX}px/${LINE_H}px Menlo,monospace;color:#7f9aa5}
    </style><pre>${ansiToHtml(frame)}</pre>`,
  );
  const png = join(OUT, `${name}.png`);
  const p = Bun.spawn(
    [CHROME, "--headless=new", "--disable-gpu", "--hide-scrollbars", "--force-device-scale-factor=2",
      `--window-size=${width},${height}`, `--screenshot=${png}`, `file://${page}`],
    { stdout: "ignore", stderr: "ignore" },
  );
  if ((await p.exited) !== 0) throw new Error(`chrome failed for ${name}`);
  console.log(`${png}  (${cols}×${lines.length})`);
}

mkdirSync(OUT, { recursive: true });
appendLog("eink-weather", demoLog);
await shoot("dashboard", <Dashboard snapshot={snapshot} selected={0} now={NOW} />);
await shoot("dashboard-fault", <Dashboard snapshot={snapshot} selected={2} now={NOW} />);
await shoot("help", <HelpView />);
await shoot("log", <LogView id="eink-weather" follow={false} />);
await shoot("services", <ServicesView stacks={demoStacks} error={null} selected={0} now={NOW} pending={new Map([["homelab/sonarr", "stopping"]])} prompt={null} />);
process.exit(0);
