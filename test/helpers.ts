import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

/** Point HQ_HOME / HQ_LAUNCH_AGENTS_DIR at fresh temp dirs so tests never touch the real system. */
export function tempHome(): string {
  const dir = mkdtempSync(join(tmpdir(), "hq-test-"));
  process.env.HQ_HOME = join(dir, "home");
  process.env.HQ_LAUNCH_AGENTS_DIR = join(dir, "agents");
  return dir;
}
