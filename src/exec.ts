import { appendFileSync, existsSync, mkdirSync } from "node:fs";
import { join } from "node:path";
import { type Exec, realExec } from "./launchd";
import { hqHome, launchAgentsDir } from "./paths";

/** HQ_FAKE_LAUNCHCTL=1: record calls instead of touching launchd (tests and dry runs). */
const fakeExec: Exec = async (args) => {
  mkdirSync(hqHome(), { recursive: true });
  appendFileSync(join(hqHome(), "launchctl.calls"), `${args.join(" ")}\n`);
  if (args[0] === "print") {
    const id = args[1]!.split("/").pop()!;
    return existsSync(join(launchAgentsDir(), `${id}.plist`))
      ? { code: 0, stdout: "\tstate = not running\n", stderr: "" }
      : { code: 113, stdout: "", stderr: "Could not find service" };
  }
  return { code: 0, stdout: "", stderr: "" };
};

export const exec: Exec = process.env.HQ_FAKE_LAUNCHCTL === "1" ? fakeExec : realExec;
