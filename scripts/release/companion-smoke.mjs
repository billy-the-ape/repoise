// Offline end-to-end smoke for the installed benchmark companion.
//
// It proves a normal installed benchmark run needs no Repoise sources:
//   1. the installed native archive (extracted outside the source tree) runs a
//      fake-adapter benchmark end to end using an installed companion package
//      (pointed at by REPOISE_BENCH_COMPANION), and its report rebuilds;
//   2. the npm launcher path resolves the same installed companion on a dry run.
//
// Everything lives in a temporary directory whose names contain spaces (so
// spaced-path resolution is exercised on every platform). No network access,
// no registry mutation, no source-checkout bridge: the native archive cannot
// walk up to a checked-in host bridge because it lives outside the checkout.
//
// Usage:
//   node scripts/release/companion-smoke.mjs <target>
//
// <target> is the platform target the native archive was packed for (the same
// value CI passes to scripts/release/pack.mjs). The native archive and the
// native platform package tarball must already exist under dist/<target>
// (pack.mjs creates them); the launcher wrapper is packed at runtime.

import { spawnSync } from "node:child_process";
import {
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  readdirSync,
  realpathSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { join } from "node:path";
import { pathToFileURL } from "node:url";
import { tmpdir } from "node:os";
import { root, targets, manifest, npm } from "./common.mjs";

const target = process.argv[2];
if (!target) throw new Error("usage: companion-smoke.mjs <target>");
const spec = targets.find((item) => item.target === target);
if (!spec) throw new Error(`unsupported target ${target}`);
const isWindows = target.endsWith("-windows-msvc");
const version = manifest().version;
const BRIDGE_VAR = "REPOISE_BENCH_HOST_BRIDGE";
const COMPANION_VAR = "REPOISE_BENCH_COMPANION";

// The per-platform dist directory produced by scripts/release/pack.mjs holds
// the native archive and the native platform package tarball. The launcher
// wrapper is packed at runtime (as the release smoke does), so no pre-existing
// dist launcher tarball is required.
const platformDistDir = join(root, "dist", target);
const archivePath = join(
  platformDistDir,
  `repoise-${version}-${target}.tar.gz`,
);
const nativePackageTarball = join(
  platformDistDir,
  `${spec.package}-${version}.tgz`,
);
for (const required of [archivePath, nativePackageTarball]) {
  if (!existsSync(required)) {
    throw new Error(
      `required artifact missing at ${required} (run scripts/release/pack.mjs first)`,
    );
  }
}

function run(command, args, options = {}) {
  const result = spawnSync(command, args, {
    cwd: root,
    stdio: "pipe",
    ...options,
  });
  if (result.error) throw result.error;
  const stdout = result.stdout.toString();
  const stderr = result.stderr.toString();
  if (result.status !== 0) {
    throw new Error(
      `${command} ${args.join(" ")} failed (exit ${result.status})\n--- stderr ---\n${stderr}\n--- stdout ---\n${stdout}`,
    );
  }
  return stdout;
}

function assert(condition, message) {
  if (!condition)
    throw new Error(`companion smoke assertion failed: ${message}`);
}

// Resolves symlinks, normalizes separators, and lowercases on Windows so the
// manifest's canonical bridge path compares equal to an installed path.
function samePath(left, right) {
  const normalize = (value) => {
    let text = String(value);
    try {
      // Prefer the native realpath (Windows): it resolves to the long form
      // without the extended-length prefix that Rust's canonicalize adds, so
      // the manifest's canonical bridge and the installed Node path compare
      // equal even when tmpdir() uses an 8.3 short name.
      text =
        typeof realpathSync.native === "function"
          ? realpathSync.native(text)
          : realpathSync(text);
    } catch {
      /* compare the literal value */
    }
    // Strip any remaining Windows extended-length prefix (\\?\) so canonical
    // and installed forms still compare equal.
    text = text.replace(/^\\\\\?\\/, "");
    text = text.replace(/\\/g, "/");
    return isWindows ? text.toLowerCase() : text;
  };
  return normalize(left) === normalize(right);
}

// A minimal self-contained fake adapter (outside the benchmarked repo): it
// reads the request line, emits hello, and a final carrying `data.answer`
// that the deterministic evaluator checks (the pattern mirrors the checked-in
// conformance fixture helpers).
const fakeAdapterSource = `#!/usr/bin/env node
let buffer = "";
const requestLine = new Promise((resolve) => {
  const onData = (chunk) => {
    buffer += chunk;
    const index = buffer.indexOf("\\n");
    if (index === -1) return;
    const line = buffer.slice(0, index);
    process.stdin.removeListener("data", onData);
    resolve(line === "" ? null : JSON.parse(line));
  };
  process.stdin.setEncoding("utf8");
  process.stdin.on("data", onData);
});
const request = await requestLine;
function emit(obj) { process.stdout.write(JSON.stringify(obj) + "\\n"); }
emit({ v: 1, t: "hello", ts: Date.now(), adapter: { name: "fake", version: "1.0.0" }, protocolVersion: 1,
  capabilities: { fresh_sessions: true, isolated_state: true, mcp_config: true, read_only_restriction: true,
    tool_telemetry: true, usage_semantics: true, model_selection: true, cancellation: true },
  usage: { eventSemantics: "delta", inputIncludesCacheReads: null, includesSubagents: null,
    includesCompaction: null, costSemantics: "provider" },
  harness: { surface: "test", revision: "1.0.0", revisionSource: "pinned" } });
emit({ v: 1, t: "final", ts: Date.now(), status: "completed", reason: null,
  usage: { inputTokens: 1, outputTokens: 1, reasoningTokens: null, cacheReadTokens: null, cacheWriteTokens: null, costUsd: null },
  durationMs: 1, exitCode: 0, data: { answer: "ok" } });
process.exit(0);
`;

const temp = mkdtempSync(join(tmpdir(), "repoise companion smoke "));
// All sub-directories carry spaces so spaced-path resolution is exercised.
const companionTarballsDir = join(temp, "dist bench-companion");
const archiveDir = join(temp, "native archive");
const consumerDir = join(temp, "consumer node modules");
const cacheDir = join(temp, "cache dir");
const repoDir = join(temp, "repo under test");
const outDir = join(temp, "bench out");
const fakeAdapter = join(temp, "fake adapter.mjs");
const suitePath = join(temp, "suite.json");

try {
  // --- 1. Extract the installed native archive outside the source tree ----
  mkdirSync(archiveDir, { recursive: true });
  run("tar", ["-xzf", archivePath, "-C", archiveDir]);
  // The archive places the binary at its root (see scripts/release/pack.mjs).
  const nativeBinary = join(archiveDir, spec.binary);
  assert(existsSync(nativeBinary), `archive binary missing at ${nativeBinary}`);

  // --- 2. Pack the companion tarballs (offline, local only) ---------------
  run("node", [
    join(root, "scripts", "release", "companion.mjs"),
    "--out",
    companionTarballsDir,
  ]);
  const tarballs = readdirSync(companionTarballsDir).filter((name) =>
    name.endsWith(".tgz"),
  );
  const protocolTarball = tarballs.find((name) =>
    name.includes("agent-adapter-protocol"),
  );
  assert(
    protocolTarball,
    `no protocol companion tarball in ${companionTarballsDir}`,
  );

  // --- 3. Install the companion into a temporary prefix (offline) --------
  mkdirSync(consumerDir, { recursive: true });
  writeFileSync(
    join(consumerDir, "package.json"),
    `${JSON.stringify({ name: "companion-consumer", version: "0.0.0", private: true }, null, 2)}\n`,
  );
  npm(
    [
      "install",
      "--offline",
      "--ignore-scripts",
      "--no-audit",
      "--no-fund",
      join(companionTarballsDir, protocolTarball),
    ],
    {
      cwd: consumerDir,
    },
  );
  const companionRoot = join(
    consumerDir,
    "node_modules",
    "@repoise",
    "agent-adapter-protocol",
  );
  const companionBridge = join(companionRoot, "bin", "adapter-host-run.mjs");
  assert(
    existsSync(companionBridge),
    `installed companion bridge missing at ${companionBridge}`,
  );

  // --- 3b. The Cline adapter tarball's runtime files load ------------------
  // companion.mjs only verifies the tarball's top-level entries, so a missing
  // or broken runtime file would otherwise first surface at trial time.
  // Installing the real Cline CLI is not part of this smoke (it needs
  // credentials), so the proof here is that every shipped bin/src module
  // parses and every src module loads (catching broken cross-module
  // imports).
  const adapterTarball = tarballs.find((name) =>
    name.includes("agent-adapter-cline"),
  );
  assert(
    adapterTarball,
    `no Cline adapter companion tarball in ${companionTarballsDir}`,
  );
  npm(
    [
      "install",
      "--offline",
      "--ignore-scripts",
      "--no-audit",
      "--no-fund",
      join(companionTarballsDir, adapterTarball),
    ],
    {
      cwd: consumerDir,
    },
  );
  const adapterRoot = join(
    consumerDir,
    "node_modules",
    "@repoise",
    "agent-adapter-cline",
  );
  for (const dir of ["bin", "src"]) {
    const dirPath = join(adapterRoot, dir);
    assert(
      existsSync(dirPath),
      `installed adapter is missing its ${dir}/ directory`,
    );
    for (const file of readdirSync(dirPath)) {
      if (!file.endsWith(".js")) continue;
      const filePath = join(dirPath, file);
      run("node", ["--check", filePath], { cwd: consumerDir });
      if (dir === "src") {
        // Load (never run) each src module: the bin entry would start a
        // trial, but the src modules are side-effect-free definitions.
        run(
          "node",
          [
            "--input-type=module",
            "-e",
            `import(${JSON.stringify(pathToFileURL(filePath).href)})`,
          ],
          {
            cwd: consumerDir,
          },
        );
      }
    }
  }

  // --- 4. A minimal benchmarked git repository (clean commit) ------------
  mkdirSync(join(repoDir, "docs"), { recursive: true });
  writeFileSync(join(repoDir, "README.md"), "# companion smoke fixture\n");
  writeFileSync(
    join(repoDir, "docs", "guide.md"),
    "# guide\n\nThe bridge answer word is ok.\n",
  );
  run("git", ["init"], { cwd: repoDir });
  run(
    "git",
    ["-c", "user.email=smoke@example.com", "-c", "user.name=Smoke", "add", "."],
    { cwd: repoDir },
  );
  run(
    "git",
    [
      "-c",
      "user.email=smoke@example.com",
      "-c",
      "user.name=Smoke",
      "commit",
      "-m",
      "fixture",
    ],
    {
      cwd: repoDir,
    },
  );

  // --- 5. The fake adapter and suite (both outside the benchmarked repo) --
  writeFileSync(fakeAdapter, fakeAdapterSource);
  const suite = {
    schemaVersion: 1,
    id: "companion-e2e",
    root: repoDir,
    snapshot: { kind: "commit", revision: "HEAD" },
    tasks: [
      {
        id: "task-a",
        prompt: "Answer with the word ok.",
        category: "exact-identifier",
        heldOut: false,
        evaluator: {
          method: "deterministic",
          answerField: "answer",
          checks: [{ id: "c1", type: "containsAll", values: ["ok"] }],
        },
      },
    ],
    conditions: [{ id: "baseline", retrievalMode: "baseline" }],
    scope: "read",
    repeat: 1,
    seed: 0,
    adapter: { executable: fakeAdapter },
    profile: { harness: { name: "fake" } },
    limits: { wallClockMs: 30000 },
    output: { dir: outDir },
  };
  writeFileSync(suitePath, `${JSON.stringify(suite, null, 2)}\n`);

  // Environment for the benchmark run: the installed companion, an isolated
  // cache, and no source override (REPOISE_BENCH_HOST_BRIDGE must not leak in).
  const env = { ...process.env };
  delete env[BRIDGE_VAR];
  env[COMPANION_VAR] = companionRoot;
  env["REPOISE_CACHE_DIR"] = cacheDir;

  // --- 6. Run the installed native archive benchmark end to end ----------
  run(nativeBinary, ["bench", suitePath, "--json"], { cwd: temp, env });
  const experimentDir = join(outDir, "companion-e2e");
  const reportPath = join(experimentDir, "report.json");
  const manifestPath = join(experimentDir, "manifest.json");
  assert(existsSync(reportPath), `report missing at ${reportPath}`);
  const report = JSON.parse(readFileSync(reportPath, "utf8"));
  assert(
    report.status === "completed",
    `benchmark did not complete: ${report.status}`,
  );
  assert(
    report.counts && report.counts.planned === 1,
    `unexpected planned count`,
  );
  assert(
    report.counts && report.counts.completed === 1,
    `expected one completed trial`,
  );
  assert(
    report.grading &&
      report.grading.overall &&
      report.grading.overall.pass === 1,
    "expected a passing grade",
  );

  // The manifest must record the installed companion as the bridge source.
  const manifest = JSON.parse(readFileSync(manifestPath, "utf8"));
  assert(
    manifest.adapter.bridgeSource === "companion",
    `bridgeSource is ${manifest.adapter.bridgeSource}`,
  );
  assert(
    samePath(manifest.adapter.bridge, companionBridge),
    `bridge ${manifest.adapter.bridge} != installed ${companionBridge}`,
  );

  // --- 7. Rebuild the report offline -------------------------------------
  run(
    nativeBinary,
    [
      "bench",
      "report",
      "--experiment",
      "companion-e2e",
      "--output",
      outDir,
      "--json",
    ],
    {
      cwd: temp,
      env,
    },
  );
  const rebuilt = JSON.parse(readFileSync(reportPath, "utf8"));
  assert(
    rebuilt.status === "completed",
    `rebuilt report status is ${rebuilt.status}`,
  );
  assert(
    rebuilt.grading &&
      rebuilt.grading.overall &&
      rebuilt.grading.overall.pass === 1,
    "rebuilt report lost its grade",
  );

  // --- 8. The npm launcher path resolves the installed companion ---------
  const launcherConsumer = join(temp, "launcher node modules");
  mkdirSync(launcherConsumer, { recursive: true });
  writeFileSync(
    join(launcherConsumer, "package.json"),
    `${JSON.stringify({ name: "launcher-consumer", version: "0.0.0", private: true }, null, 2)}\n`,
  );
  // Pack the thin launcher wrapper at runtime (as the release smoke does),
  // then install the launcher, the native platform package, and the installed
  // companion into the same isolated prefix.
  npm([
    "pack",
    join(root, "npm", "repoise"),
    "--ignore-scripts",
    "--pack-destination",
    launcherConsumer,
  ]);
  const wrapperTarball = join(launcherConsumer, `repoise-${version}.tgz`);
  assert(
    existsSync(wrapperTarball),
    `launcher wrapper missing at ${wrapperTarball}`,
  );
  npm(
    [
      "install",
      "--offline",
      "--ignore-scripts",
      "--no-audit",
      "--no-fund",
      "--omit=optional",
      wrapperTarball,
      nativePackageTarball,
      join(companionTarballsDir, protocolTarball),
    ],
    {
      cwd: launcherConsumer,
    },
  );
  const launcher = join(
    launcherConsumer,
    "node_modules",
    "repoise",
    "bin",
    "repoise.cjs",
  );
  assert(existsSync(launcher), `launcher entry missing at ${launcher}`);

  const dryRun = run(
    "node",
    [launcher, "bench", suitePath, "--dry-run", "--json"],
    { cwd: temp, env },
  );
  const dryRunDoc = JSON.parse(dryRun);
  const dryBridge = dryRunDoc.prerequisites && dryRunDoc.prerequisites.bridge;
  assert(
    dryBridge && dryBridge.source === "companion",
    `launcher dry-run bridge source is ${dryBridge && dryBridge.source}`,
  );
  assert(
    samePath(dryBridge.path, companionBridge),
    `launcher dry-run bridge ${dryBridge.path} != installed`,
  );
} finally {
  rmSync(temp, { recursive: true, force: true });
}
