import { describe, expect, test } from "bun:test";
import { homedir } from "node:os";
import { ConfigError, parseConfig } from "../src/config";
import {
  composeArgs, type DockerExec, isServiceFault, loadStackView, parseBytes, parseComposeConfig, parseStacks, parseStats,
  serviceUrl, totals,
} from "../src/stacks";
import { shortUptime } from "../src/tui/ServicesView";

describe("parseStacks", () => {
  test("reads dir, files and env_file relative to dir", () => {
    const [s] = parseStacks('[stacks.terminus]\ndir = "~/x"\nfiles = ["a/compose.yml", "/abs/o.yml"]\nenv_file = "a/.env"\n');
    expect(s).toEqual({ name: "terminus", dir: `${homedir()}/x`, files: [`${homedir()}/x/a/compose.yml`, "/abs/o.yml"], envFile: `${homedir()}/x/a/.env` });
  });

  test("no stacks table is fine", () => expect(parseStacks('[jobs.a]\ncommand = "x"\nevery = "5m"\n')).toEqual([]));

  test("rejects unknown keys and a missing dir", () => {
    expect(() => parseStacks('[stacks.a]\ndir = "/x"\nport = 1\n')).toThrow(ConfigError);
    expect(() => parseStacks("[stacks.a]\n")).toThrow("stacks.a: dir is required");
  });

  test("jobs config accepts a stacks table and ignores it", () => {
    expect(parseConfig('[stacks.a]\nbogus = 1\n\n[jobs.j]\ncommand = "x"\nevery = "5m"\n').map((j) => j.id)).toEqual(["j"]);
  });
});

describe("job url", () => {
  test("parsed and validated", () => {
    expect(parseConfig('[jobs.j]\ncommand = "x"\nmanual = true\nurl = "http://localhost:8742/"\n')[0]!.url).toBe("http://localhost:8742/");
    expect(parseConfig('[jobs.j]\ncommand = "x"\nmanual = true\n')[0]!.url).toBeNull();
    expect(() => parseConfig('[jobs.j]\ncommand = "x"\nmanual = true\nurl = "localhost:1"\n')).toThrow("url must start with");
  });
});

describe("docker output parsing", () => {
  test("parseBytes", () => {
    expect(parseBytes("864KiB")).toBe(864 * 1024);
    expect(parseBytes("38.5MiB")).toBe(38.5 * 1024 ** 2);
    expect(parseBytes("1.2GB")).toBe(1.2e9);
    expect(parseBytes("nope")).toBeNull();
  });

  test("parseStats", () => {
    const out = parseStats(
      '{"CPUPerc":"12.50%","MemUsage":"38.67MiB / 256MiB","Name":"jellyfin"}\n{"CPUPerc":"--","MemUsage":"-- / --","Name":"dead"}\n',
    );
    expect(out.get("jellyfin")).toEqual({ cpu: 12.5, mem: 38.67 * 1024 ** 2 });
    expect(out.has("dead")).toBe(false);
  });

  test("parseComposeConfig keeps tcp published ports in order", () => {
    const d = parseComposeConfig(JSON.stringify({ services: {
      jellyfin: { image: "jellyfin/jellyfin", container_name: "jellyfin", ports: [
        { target: 8096, published: "8096", protocol: "tcp" }, { target: 1900, published: "1900", protocol: "udp" }, { target: 8920, published: "8920" },
      ] },
      cloudflared: { image: "cloudflare/cloudflared", ports: null },
    } }));
    expect([...d.keys()]).toEqual(["jellyfin", "cloudflared"]);
    expect(d.get("jellyfin")!.ports).toEqual([8096, 8920]);
    expect(d.get("cloudflared")!.ports).toEqual([]);
  });

  test("shortUptime", () => {
    expect(shortUptime("Up 39 hours")).toBe("39h");
    expect(shortUptime("Up 39 hours (healthy)")).toBe("39h");
    expect(shortUptime("Up About an hour")).toBe("1h");
    expect(shortUptime("Up Less than a second")).toBe("<1s");
    expect(shortUptime("Up 2 days")).toBe("2d");
  });
});

describe("loadStackView", () => {
  const stack = { name: "lab", dir: "/nonexistent-lab", files: [], envFile: null };
  const config = JSON.stringify({ services: {
    jellyfin: { image: "jellyfin/jellyfin", ports: [{ target: 8096, published: "8096", protocol: "tcp" }] },
    radarr: { image: "radarr", ports: [{ target: 7878, published: "7878" }] },
    sonarr: { image: "sonarr", ports: [] },
  } });
  const ps = [
    { Service: "jellyfin", Name: "jellyfin", State: "running", Health: "", Status: "Up 2 hours", ExitCode: 0 },
    { Service: "radarr", Name: "radarr", State: "exited", Health: "", Status: "Exited (1) 5 minutes ago", ExitCode: 1 },
  ].map((r) => JSON.stringify(r)).join("\n");
  const calls: string[][] = [];
  const docker: DockerExec = async (args) => {
    calls.push(args);
    if (args.includes("config")) return { code: 0, stdout: config, stderr: "" };
    if (args.includes("ps")) return { code: 0, stdout: ps, stderr: "" };
    return { code: 1, stdout: "", stderr: "unexpected" };
  };

  test("merges declared services, containers and usage", async () => {
    const v = await loadStackView(stack, docker, new Map([["jellyfin", { cpu: 80, mem: 2 * 1024 ** 3 }]]));
    expect(v.error).toBeNull();
    const [jf, ra, so] = v.services;
    expect(jf).toMatchObject({ service: "jellyfin", state: "running", cpu: 80, ports: [8096] });
    expect(serviceUrl(jf!)).toBe("http://localhost:8096");
    expect(ra).toMatchObject({ state: "exited", exitCode: 1 });
    expect(isServiceFault(ra!)).toBe(true);
    expect(so).toMatchObject({ state: "missing", container: null, cpu: null });
    expect(serviceUrl(so!)).toBeNull();
    expect(totals(v.services)).toEqual({ up: 1, all: 3, cpu: 80, mem: 2 * 1024 ** 3 });
    expect(calls[0]).toEqual(["compose", "--project-directory", "/nonexistent-lab", "config", "--format", "json"]);
  });

  test("a compose error is reported on the stack, not thrown", async () => {
    const v = await loadStackView({ ...stack, name: "broken" }, async () => ({ code: 1, stdout: "", stderr: "warn\nno such file\n" }), new Map());
    expect(v.error).toBe("no such file");
  });

  test("composeArgs adds env file and compose files", () => {
    expect(composeArgs({ name: "t", dir: "/r", files: ["/r/a.yml", "/r/b.yml"], envFile: "/r/.env" }, "ps")).toEqual([
      "compose", "--project-directory", "/r", "--env-file", "/r/.env", "-f", "/r/a.yml", "-f", "/r/b.yml", "ps",
    ]);
  });
});
