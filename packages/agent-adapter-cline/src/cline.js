// Building and resolving the Cline CLI invocation for one trial.
//
// Flag mapping follows the Cline headless CLI reference (verified
// 2026-10-19): `--json` NDJSON output, `-c` workspace, `--config`/
// `--data-dir` for isolated trial state, `CLINE_MCP_SETTINGS_PATH` for the
// per-trial MCP server list, `--auto-approve`, `-m`/`-P` model/provider,
// `--thinking`, `--compaction`, `-t` wall-clock backstop in seconds, `-k`
// per-run API key. The prompt travels on stdin, never argv.

import { spawnSync } from "node:child_process";
import { existsSync, readFileSync } from "node:fs";
import { join } from "node:path";

// Returns a launch spec `{ executable, prefixArgs }`: `executable` plus
// arguments that precede the CLI argv. Explicit `.js`/`.mjs`/`.cjs` paths
// are launched through the Node binary itself, because Windows
// CreateProcess cannot execute shebang scripts and has no `.mjs`
// extension association.
export function resolveClineExecutable() {
  const explicit = process.env.CLONE_EXECUTABLE || process.env.CLINE_EXECUTABLE;
  if (explicit) {
    if (explicit.includes("/") || explicit.includes("\\")) {
      if (!existsSync(explicit))
        return { executable: null, prefixArgs: [], error: "cline_not_found" };
    }
    if (/\.(?:js|mjs|cjs)$/i.test(explicit)) {
      return {
        executable: process.execPath,
        prefixArgs: [explicit],
        error: null,
      };
    }
    return { executable: explicit, prefixArgs: [], error: null };
  }
  // Bare command: resolved through the adapter process PATH at spawn time.
  // On Windows the npm `.cmd` shim cannot be spawned without a shell, so
  // point CLONE_EXECUTABLE/CLINE_EXECUTABLE at the script or a native
  // executable there (see docs/agent-adapters.md).
  return { executable: "cline", prefixArgs: [], error: null };
}

/** Best-effort `cline version` probe; null when unavailable. */
export function probeClineVersion(launch) {
  const result = spawnSync(
    launch.executable,
    [...launch.prefixArgs, "version"],
    {
      env: {
        PATH: process.env.PATH,
        HOME: process.env.HOME,
        USERPROFILE: process.env.USERPROFILE,
        SystemRoot: process.env.SystemRoot,
      },
      encoding: "utf8",
      timeout: 10_000,
      stdio: ["ignore", "pipe", "pipe"],
      shell: false,
    },
  );
  if (result.status !== 0) return null;
  const text = (result.stdout ?? "").trim();
  const match = text.match(/\d+\.\d+\.\d+(?:[-+][\w.]+)?/);
  return match ? match[0] : text.length > 0 ? text : null;
}

/** Build the argv, environment and prompt for the trial's cline run.
 * The raw credential value never enters argv here; the caller receives a
 * `credential` object and appends the key flag itself at spawn time. */
export function buildInvocation(
  request,
  { configDir, dataDir, mcpSettingsPath },
) {
  const argv = [
    "--json",
    "-c",
    request.workspace.root,
    "--config",
    configDir,
    "--data-dir",
    dataDir,
    "--auto-approve",
    "true",
  ];
  const model = request.profile.model;
  if (model && model.id) argv.push("-m", model.id);
  const provider = request.profile.provider;
  if (provider && provider.id) argv.push("-P", provider.id);
  const reasoning = request.profile.reasoning;
  if (reasoning && reasoning.level && reasoning.level !== "none") {
    argv.push("--thinking", reasoning.level);
  }
  const compaction = request.profile.compaction;
  if (compaction && compaction.mode && compaction.mode !== "off") {
    argv.push("--compaction", compaction.mode);
  }
  const wallClockMs = request.limits?.wallClockMs;
  if (typeof wallClockMs === "number" && wallClockMs > 0) {
    argv.push("-t", String(Math.ceil(wallClockMs / 1000)));
  }

  let credential = null;
  const ref = provider?.credentialRef;
  if (typeof ref === "string" && ref.startsWith("env:")) {
    const name = ref.slice(4);
    if (process.env[name]) credential = { name, source: ref };
  }

  // Per-trial MCP configuration. The isolated --config dir guarantees no
  // global servers leak into the trial; baseline mode gets an empty list.
  const mcpServers = {};
  const mcp = request.condition.mcp;
  if (
    request.condition.retrievalMode === "repoise" &&
    mcp &&
    mcp.argv.length > 0
  ) {
    mcpServers[mcp.serverName] = {
      command: mcp.argv[0],
      args: mcp.argv.slice(1),
      env: mcp.env ?? {},
    };
  }

  // The prompt goes on stdin (documented CLI pattern), never argv.
  let prompt;
  if (typeof request.task.prompt === "string") {
    prompt = request.task.prompt;
  } else if (typeof request.task.promptFile === "string") {
    try {
      prompt = readFileSync(request.task.promptFile, "utf8");
    } catch {
      prompt = null;
    }
  } else {
    prompt = "";
  }

  // Minimal environment pass-through: never the whole inherited
  // environment, and credential values only through the named variable.
  const env = {};
  for (const name of ["PATH", "HOME", "USERPROFILE", "SystemRoot", "TMPDIR"]) {
    if (process.env[name]) env[name] = process.env[name];
  }
  // Documented adapter-specific passthrough namespace (e.g. test stubs).
  for (const [name, value] of Object.entries(process.env)) {
    if (name.startsWith("CLONE_")) env[name] = value;
  }
  env.CLINE_MCP_SETTINGS_PATH = mcpSettingsPath;
  env.CLINE_LOG_PATH = join(dataDir, "logs", "cline.log");
  if (credential) env[credential.name] = process.env[credential.name];

  return { argv, env, prompt, mcpServers, credential };
}
