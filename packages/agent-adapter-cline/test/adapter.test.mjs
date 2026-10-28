// Adapter tests: the Cline adapter is driven through the real protocol host
// (runAdapter) with a stub Cline CLI, so the full stdin/stdout boundary is
// exercised without live inference.
import test from "node:test";
import assert from "node:assert/strict";
import {
  mkdtempSync,
  mkdirSync,
  readFileSync,
  chmodSync,
  existsSync,
  statSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import Ajv from "ajv";
import { runAdapter } from "../../agent-adapter-protocol/src/index.js";
import { resolveClineExecutable } from "../src/cline.js";

const here = dirname(fileURLToPath(import.meta.url));
const adapterBin = join(here, "..", "bin", "agent-adapter-cline.js");
const stub = join(here, "fixtures", "stub-cline.mjs");
const schemasDir = join(here, "..", "..", "..", "schemas");
chmodSync(stub, 0o755);

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

function makeRoot() {
  const root = mkdtempSync(join(tmpdir(), "ap03-cline-"));
  const workspace = join(root, "workspace");
  mkdirSync(workspace, { recursive: true });
  const artifacts = join(root, "artifacts");
  return { root, workspace, artifacts };
}

function makeRequest({
  workspace,
  artifacts,
  requestOverrides: overrides = {},
}) {
  const base = {
    protocolVersion: 1,
    identity: {
      experimentId: "exp-1",
      trialId: "trial-1",
      taskId: "task-1",
      replication: 0,
    },
    workspace: {
      root: workspace,
      revision: "git:abc123",
      scope: "read",
      allowedWritePaths: [],
    },
    task: {
      prompt: "Say hello to the workspace",
      promptFile: null,
      taskVersion: "1",
      taskHash: "sha256:test",
    },
    condition: {
      retrievalMode: "repoise",
      requiredCapabilities: [],
      mcp: { serverName: "repoise", argv: ["repoise", "mcp"], env: {} },
      guidanceOverlay: null,
    },
    profile: {
      harness: { name: "cline", revision: null },
      provider: { id: "stub-provider", credentialRef: "env:STUB_API_KEY" },
      model: { id: "stub-model" },
      reasoning: { level: "low" },
      compaction: { mode: "basic" },
      sampling: null,
    },
    limits: { wallClockMs: 60_000 },
    outputs: {
      artifactDir: artifacts,
      eventsFile: join(artifacts, "events.ndjson"),
      resultFile: join(artifacts, "result.json"),
    },
  };
  const merged = { ...base, ...overrides };
  merged.workspace = { ...base.workspace, ...(overrides.workspace ?? {}) };
  merged.task = { ...base.task, ...(overrides.task ?? {}) };
  merged.condition = { ...base.condition, ...(overrides.condition ?? {}) };
  merged.profile = { ...base.profile, ...(overrides.profile ?? {}) };
  merged.limits = { ...base.limits, ...(overrides.limits ?? {}) };
  return merged;
}

async function runAdapterTrial({
  workspace,
  artifacts,
  requestOverrides,
  stubMode = "ok",
  signal,
  extraEnv,
}) {
  const request = makeRequest({ workspace, artifacts, requestOverrides });
  const result = await runAdapter(request, {
    executable: process.execPath,
    args: [adapterBin],
    ...(signal ? { signal } : {}),
    env: {
      CLINE_EXECUTABLE: stub,
      CLONE_STUB_MODE: stubMode,
      STUB_API_KEY: "test-key-123",
      ...(extraEnv ?? {}),
    },
  });
  return { request, result };
}

test("repoise mode: isolated state, MCP config, effective-config, usage", async () => {
  const { workspace, artifacts } = makeRoot();
  const { result } = await runAdapterTrial({ workspace, artifacts });

  assert.equal(result.status, "completed");
  assert.equal(result.adapter.name, "cline");
  assert.equal(result.harness.revisionSource, "runtime");
  assert.equal(result.events[0].t, "hello");
  const hello = result.events[0];
  assert.equal(hello.capabilities.tool_telemetry, false);
  assert.equal(hello.capabilities.read_only_restriction, false);
  assert.equal(hello.capabilities.mcp_config, true);
  assert.equal(hello.usage.eventSemantics, "cumulative");
  assert.equal(result.usage.inputTokens, 10);
  assert.equal(result.usage.outputTokens, 25);
  assert.equal(result.usage.costUsd, 0.003);
  assert.equal(result.adapterDurationMs, 42);
  assert.equal(result.exitCode, 0);

  // The final event records the agent's final message text (the stub's
  // `done` text) under `data.answer` for benchmark grading.
  const finalEvent = result.events.at(-1);
  assert.equal(finalEvent.t, "final");
  assert.equal(finalEvent.data.answer, "stub answer");

  // The stub records the invocation it actually saw.
  const manifest = JSON.parse(
    readFileSync(join(workspace, "stub-manifest.json"), "utf8"),
  );
  const argv = manifest.argv;
  assert.ok(argv[0] === "--json");
  const workspaceIndex = argv.indexOf("-c");
  assert.equal(argv[workspaceIndex + 1], workspace);
  const configIndex = argv.indexOf("--config");
  assert.ok(
    argv[configIndex + 1].startsWith(artifacts),
    "config dir must be trial-local",
  );
  const dataIndex = argv.indexOf("--data-dir");
  assert.ok(
    argv[dataIndex + 1].startsWith(artifacts),
    "data dir must be trial-local",
  );
  assert.deepEqual(argv.slice(argv.indexOf("-m"), argv.indexOf("-m") + 2), [
    "-m",
    "stub-model",
  ]);
  assert.deepEqual(argv.slice(argv.indexOf("-P"), argv.indexOf("-P") + 2), [
    "-P",
    "stub-provider",
  ]);
  assert.deepEqual(
    argv.slice(argv.indexOf("--thinking"), argv.indexOf("--thinking") + 2),
    ["--thinking", "low"],
  );
  assert.deepEqual(
    argv.slice(argv.indexOf("--compaction"), argv.indexOf("--compaction") + 2),
    ["--compaction", "basic"],
  );
  assert.deepEqual(argv.slice(argv.indexOf("-t"), argv.indexOf("-t") + 2), [
    "-t",
    "60",
  ]);
  const keyIndex = argv.indexOf("-k");
  assert.ok(
    keyIndex !== -1,
    "the credential travels to the CLI through -k only",
  );
  assert.equal(argv[keyIndex + 1], "test-key-123");
  assert.equal(
    manifest.prompt,
    "Say hello to the workspace",
    "prompt travels on stdin",
  );
  assert.ok(manifest.mcpSettingsPath.startsWith(artifacts));

  // The per-trial MCP settings file configures only the repoise server.
  const mcpSettings = JSON.parse(
    readFileSync(manifest.mcpSettingsPath, "utf8"),
  );
  assert.deepEqual(mcpSettings.mcpServers, {
    repoise: { command: "repoise", args: ["mcp"], env: {} },
  });

  // The effective configuration is an immutable artifact and redacted.
  const configArtifacts = result.events.filter((e) => e.t === "artifact");
  assert.ok(configArtifacts.some((a) => a.kind === "effective-config"));
  const effectivePath = configArtifacts.find(
    (a) => a.kind === "effective-config",
  ).path;
  const effectiveText = readFileSync(effectivePath, "utf8");
  const effective = JSON.parse(effectiveText);
  assert.ok(
    !effectiveText.includes("test-key-123"),
    "credential values must stay redacted",
  );
  assert.equal(effective.condition.mcp[0].serverName, "repoise");
  assert.equal(
    effective.task.promptLength,
    "Say hello to the workspace".length,
  );
  assert.ok(/^sha256:/.test("sha256:" + effective.task.promptSha256));
  assert.equal(effective.profile.provider.credentialPresent, true);

  // The version probe surfaces the harness revision as a note.
  const versionNote = result.events.find(
    (e) => e.t === "note" && e.code === "harness_version",
  );
  assert.equal(versionNote.data.revision, "1.0.0-stub");
});

test("baseline mode: no MCP servers, no provider, no credential", async () => {
  const { workspace, artifacts } = makeRoot();
  const { result } = await runAdapterTrial({
    workspace,
    artifacts,
    requestOverrides: {
      condition: { retrievalMode: "baseline", mcp: null },
      profile: {
        provider: null,
        model: null,
        reasoning: null,
        compaction: null,
      },
    },
  });
  assert.equal(result.status, "completed");
  const manifest = JSON.parse(
    readFileSync(join(workspace, "stub-manifest.json"), "utf8"),
  );
  assert.equal(manifest.argv.includes("-P"), false);
  assert.equal(manifest.argv.includes("-k"), false);
  assert.equal(manifest.argv.includes("-m"), false);
  const mcpSettings = JSON.parse(
    readFileSync(manifest.mcpSettingsPath, "utf8"),
  );
  assert.deepEqual(mcpSettings.mcpServers, {});
  const effective = JSON.parse(
    readFileSync(
      result.events.find(
        (a) => a.t === "artifact" && a.kind === "effective-config",
      ).path,
      "utf8",
    ),
  );
  assert.equal(effective.condition.mcp, null);
});

test("host cancellation maps to final cancelled", async () => {
  const { workspace, artifacts } = makeRoot();
  const controller = new AbortController();
  const pending = runAdapterTrial({
    workspace,
    artifacts,
    stubMode: "hang",
    signal: controller.signal,
  });
  setTimeout(() => controller.abort(), 400);
  const { result } = await pending;
  assert.equal(result.status, "cancelled");
  assert.equal(result.events.at(-1).t, "final");
});

test("a cancel in the same stdin chunk as the request is not lost", async () => {
  const { PassThrough } = await import("node:stream");
  const { attachStdinLines } = await import("../src/main.js");
  const stream = new PassThrough();
  const controlLines = [];
  const requestReady = new Promise((resolve) => {
    attachStdinLines(stream, resolve, (line) => controlLines.push(line));
  });
  // One chunk: the request line followed by the cancel line, exactly the
  // shape a pre-aborted host signal produces (request + `cancel` back to
  // back).
  stream.write(`${JSON.stringify({ protocolVersion: 1 })}\ncancel\n`);
  const requestLine = await requestReady;
  assert.deepEqual(JSON.parse(requestLine), { protocolVersion: 1 });
  // The control line was consumed from the same chunk, not discarded.
  assert.deepEqual(controlLines, ["cancel"]);
});

test("a grandchild in the cline group is swept with the trial", async () => {
  const { root, workspace, artifacts } = makeRoot();
  const heartbeatFile = join(root, "heartbeat.txt");
  const { result } = await runAdapterTrial({
    workspace,
    artifacts,
    stubMode: "leak",
    extraEnv: { CLONE_LEAK_HB_FILE: heartbeatFile },
  });
  assert.equal(result.status, "completed");
  if (process.platform === "win32") return; // group kill is POSIX-only
  const size = existsSync(heartbeatFile) ? statSync(heartbeatFile).size : 0;
  assert.ok(size > 0, "the grandchild heartbeat never ran");
  // It must stop once the adapter swept the child group on the final.
  await sleep(300);
  assert.equal(
    statSync(heartbeatFile).size,
    size,
    "grandchild kept writing after the trial ended",
  );
});

test("failed cline child without run_result maps to adapter_error", async () => {
  const { workspace, artifacts } = makeRoot();
  const { result } = await runAdapterTrial({
    workspace,
    artifacts,
    stubMode: "fail",
  });
  assert.equal(result.status, "adapter_error");
  assert.ok(result.reason.startsWith("no_run_result"));
  assert.ok(result.warnings.some((w) => w.code === "cline_stderr_error"));
});

test("required capability the CLI cannot provide fails preflight", async () => {
  const { workspace, artifacts } = makeRoot();
  const { result } = await runAdapterTrial({
    workspace,
    artifacts,
    requestOverrides: {
      condition: { requiredCapabilities: ["read_only_restriction"] },
    },
  });
  assert.equal(result.status, "protocol_error");
  assert.equal(result.reason, "missing_required_capability");
});

test("a non-env credentialRef is rejected before any effective-config", async () => {
  const { workspace, artifacts } = makeRoot();
  const { result } = await runAdapterTrial({
    workspace,
    artifacts,
    requestOverrides: {
      profile: {
        provider: { id: "stub-provider", credentialRef: "sk-raw-key-123" },
      },
    },
  });
  assert.equal(result.status, "protocol_error");
  assert.equal(result.reason, "invalid_request");
});

test("a credential echoed by the CLI is scrubbed from all events", async () => {
  const { workspace, artifacts } = makeRoot();
  const secret = "sk-echoed-secret-xyz789";
  const { result } = await runAdapterTrial({
    workspace,
    artifacts,
    requestOverrides: {
      profile: {
        provider: { id: "stub-provider", credentialRef: "env:CLINE_TEST_KEY" },
      },
    },
    extraEnv: { CLINE_TEST_KEY: secret, CLONE_ECHO_KEY: "1" },
  });
  assert.equal(result.status, "completed");
  const serialized = JSON.stringify(result.events);
  assert.ok(!serialized.includes(secret), "credential leaked into events");
  assert.ok(serialized.includes("[REDACTED]"), "redaction marker missing");
  // The stub also emits an unparseable line whose key straddles the 200-char
  // prefix boundary: it must be redacted, not truncated mid-key.
  const unparsed = result.events.find((e) => e.code === "cline_unparsed_line");
  assert.ok(unparsed, "straddling unparsed line event missing");
  assert.ok(
    unparsed.data.prefix.includes("[REDACTED]"),
    "straddling key not redacted",
  );
});

test("explicit Node script executables launch through the Node binary", () => {
  const previous = process.env.CLINE_EXECUTABLE;
  delete process.env.CLONE_EXECUTABLE;
  process.env.CLINE_EXECUTABLE = stub;
  try {
    const { executable, prefixArgs, error } = resolveClineExecutable();
    assert.equal(error, null);
    assert.equal(executable, process.execPath);
    assert.deepEqual(prefixArgs, [stub]);
  } finally {
    if (previous === undefined) delete process.env.CLINE_EXECUTABLE;
    else process.env.CLINE_EXECUTABLE = previous;
  }
});

test("no explicit executable resolves to the bare cline command", () => {
  const previous = process.env.CLINE_EXECUTABLE;
  const previousClone = process.env.CLONE_EXECUTABLE;
  delete process.env.CLINE_EXECUTABLE;
  delete process.env.CLONE_EXECUTABLE;
  try {
    assert.deepEqual(resolveClineExecutable(), {
      executable: "cline",
      prefixArgs: [],
      error: null,
    });
  } finally {
    if (previous === undefined) delete process.env.CLINE_EXECUTABLE;
    else process.env.CLINE_EXECUTABLE = previous;
    if (previousClone === undefined) delete process.env.CLONE_EXECUTABLE;
    else process.env.CLONE_EXECUTABLE = previousClone;
  }
});

test("missing explicit executable maps to adapter_error cline_not_found", async () => {
  const { root, workspace, artifacts } = makeRoot();
  const request = makeRequest({ workspace, artifacts });
  const result = await runAdapter(request, {
    executable: process.execPath,
    args: [adapterBin],
    env: { CLINE_EXECUTABLE: join(root, "does-not-exist-cline") },
  });
  assert.equal(result.status, "adapter_error");
  assert.equal(result.reason, "cline_not_found");
});

test("checked-in schemas accept the adapter's own request and events", async () => {
  const { workspace, artifacts } = makeRoot();
  const { request, result } = await runAdapterTrial({ workspace, artifacts });
  assert.equal(result.status, "completed");
  const ajv = new Ajv({ validateSchema: false, strict: false });
  const validateRequest = ajv.compile(
    JSON.parse(
      readFileSync(join(schemasDir, "adapter-request.v1.schema.json"), "utf8"),
    ),
  );
  const validateEvent = ajv.compile(
    JSON.parse(
      readFileSync(join(schemasDir, "adapter-events.v1.schema.json"), "utf8"),
    ),
  );
  assert.ok(
    validateRequest(request),
    `request: ${JSON.stringify(validateRequest.errors)}`,
  );
  for (const event of result.events) {
    assert.ok(
      validateEvent(event),
      `${event.t}: ${JSON.stringify(validateEvent.errors)}`,
    );
  }
  // The host's protocol-violation backstop reads this recorded child pid.
  const recorded = Number(
    readFileSync(join(artifacts, "child-pid.txt"), "utf8").trim(),
  );
  assert.ok(Number.isInteger(recorded) && recorded > 0);
});
