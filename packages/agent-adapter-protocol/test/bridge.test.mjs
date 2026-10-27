// AP-04 host bridge tests: the Node bridge that `repoise bench` spawns is
// driven exactly like the Rust coordinator drives it — request line on
// stdin, cooperative `cancel` line later, result.json produced by the
// host. No live inference; deterministic fake fixtures only.
import test from "node:test";
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { mkdtempSync, readFileSync, existsSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const fixtures = join(here, "fixtures");
const bridge = join(here, "..", "bin", "adapter-host-run.mjs");

function makeRequest(root) {
  return {
    protocolVersion: 1,
    identity: {
      experimentId: "exp-bridge",
      trialId: "trial-1",
      taskId: "task-1",
      replication: 0,
    },
    workspace: {
      root: "/tmp/ap04-workspace",
      revision: null,
      scope: "read",
      allowedWritePaths: [],
    },
    task: {
      prompt: "Say hello",
      promptFile: null,
      taskVersion: "1",
      taskHash: "sha256:test",
    },
    condition: {
      retrievalMode: "baseline",
      requiredCapabilities: [],
      mcp: null,
      guidanceOverlay: null,
    },
    profile: {
      harness: { name: "fake", revision: null },
      provider: null,
      model: null,
      reasoning: null,
      compaction: null,
      sampling: null,
    },
    limits: { wallClockMs: 15_000 },
    outputs: {
      artifactDir: join(root, "artifacts"),
      eventsFile: join(root, "events.ndjson"),
      resultFile: join(root, "result.json"),
    },
  };
}

function runBridge(fixture, { request, cancelAfterMs = null } = {}) {
  const root = mkdtempSync(join(tmpdir(), "ap04-bridge-"));
  const requestJson = JSON.stringify(request ?? makeRequest(root));
  const child = spawn(process.execPath, [bridge, join(fixtures, fixture)], {
    stdio: ["pipe", "pipe", "pipe"],
  });
  child.stdin.write(`${requestJson}\n`);
  if (cancelAfterMs !== null) {
    setTimeout(() => child.stdin.write("cancel\n"), cancelAfterMs);
  }
  return new Promise((resolve, reject) => {
    let stderr = "";
    child.stderr.on("data", (chunk) => (stderr += chunk));
    child.on("error", reject);
    child.on("close", (code) => resolve({ root, code, stderr }));
  });
}

test("bridge completes a successful trial and writes host artifacts", async () => {
  const { root, code } = await runBridge("fake-ok.mjs");
  assert.equal(code, 0, "the bridge exits 0 once the host produced a result");
  const result = JSON.parse(readFileSync(join(root, "result.json"), "utf8"));
  assert.equal(result.status, "completed");
  assert.equal(result.adapter.name, "fake");
  assert.equal(result.exitCode, 0);
  const events = readFileSync(join(root, "events.ndjson"), "utf8")
    .trim()
    .split("\n");
  assert.equal(events.length, result.events.length);
  assert.equal(events.at(-1), JSON.stringify(result.events.at(-1)));
});

test("bridge mirrors adapter stderr to the artifact directory", async () => {
  const { root, code } = await runBridge("fake-stderr.mjs");
  assert.equal(code, 0);
  const stderrLog = readFileSync(join(root, "artifacts", "stderr.log"), "utf8");
  assert.match(stderrLog, /fake adapter stderr line/);
});

test("bridge forwards the cooperative cancel line to the adapter", async () => {
  const { root, code } = await runBridge("fake-cancel.mjs", {
    cancelAfterMs: 500,
  });
  assert.equal(code, 0);
  const result = JSON.parse(readFileSync(join(root, "result.json"), "utf8"));
  assert.equal(result.status, "cancelled");
  assert.equal(result.reason, "host_requested");
});

test("bridge honors a cancel line arriving in the same chunk as the request", async () => {
  // The coordinator writes the request, then (if SIGINT was already flagged
  // during trial setup) the cancel line within microseconds; both can land in
  // the pipe before the bridge attaches its first stdin listener. The cancel
  // must not be dropped.
  const root = mkdtempSync(join(tmpdir(), "ap04-bridge-"));
  const requestJson = JSON.stringify(makeRequest(root));
  const child = spawn(process.execPath, [bridge, join(fixtures, "fake-cancel.mjs")], {
    stdio: ["pipe", "pipe", "pipe"],
  });
  child.stdin.write(`${requestJson}\ncancel\n`);
  const code = await new Promise((resolve) => child.on("close", resolve));
  assert.equal(code, 0);
  const result = JSON.parse(readFileSync(join(root, "result.json"), "utf8"));
  assert.equal(result.status, "cancelled");
  assert.equal(result.reason, "host_requested");
});

test("bridge fails when the request line never arrives", async () => {
  const root = mkdtempSync(join(tmpdir(), "ap04-bridge-"));
  const child = spawn(
    process.execPath,
    [bridge, join(fixtures, "fake-ok.mjs")],
    {
      stdio: ["pipe", "pipe", "pipe"],
    },
  );
  child.stdin.end();
  const code = await new Promise((resolve) => {
    child.on("close", resolve);
  });
  assert.notEqual(code, 0, "a missing request line must not succeed");
  assert.ok(!existsSync(join(root, "result.json")));
});
