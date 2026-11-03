import assert from "node:assert/strict";
import {
  mkdtempSync,
  mkdirSync,
  writeFileSync,
  rmSync,
  readdirSync,
  readFileSync,
  existsSync,
  realpathSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { spawn, spawnSync } from "node:child_process";
import readline from "node:readline";
import { root, targets, manifest, npm, run } from "./common.mjs";
import {
  checkAp07Handshake,
  validateEventStream,
  handshakeFixtures,
  MACHINE_SCHEMA_FILES,
} from "./ap07-contract.mjs";

const spec = targets.find((item) => item.target === process.argv[2]);
if (!spec) throw new Error("Pass a supported target");
const directory = resolve(root, process.argv[3] ?? `dist/${spec.target}`);
const CLI_TIMEOUT_MS = 60_000;
const MCP_TIMEOUT_MS = 30_000;
const MCP_MAX_BUFFER = 4 * 1024 * 1024;
const QUERY = "quasarledger";
const DOCS_PATH = "docs/guide.md";
const CODE_TS_PATH = "src/calculator.ts";
const CODE_JS_PATH = "src/legacy.js";
const SCHEMA_FILE = "schemas/repoise.config.v1.schema.json";
const PROTOCOL_VERSION = "2025-06-18";
const WATCH_TIMEOUT_MS = 30_000;

/// The synthetic fixture both installed forms must index: one Markdown doc and
/// one TypeScript/JavaScript source each, sharing one unique lexical token.
function writeFixture(fixture) {
  mkdirSync(join(fixture, "docs"), { recursive: true });
  mkdirSync(join(fixture, "src"), { recursive: true });
  writeFileSync(
    join(fixture, DOCS_PATH),
    [
      "# Quasar Ledger Guide",
      "",
      "The quasarledger service reconciles offline corpus records.",
      "",
      "Publish a generation with the index command.",
      "",
    ].join("\n"),
  );
  writeFileSync(
    join(fixture, CODE_TS_PATH),
    [
      "export function quasarledger(values: number[]): number {",
      "  return values.reduce((sum, value) => sum + value, 0);",
      "}",
      "",
    ].join("\n"),
  );
  writeFileSync(
    join(fixture, CODE_JS_PATH),
    [
      "function quasarledger(items) {",
      "  return items;",
      "}",
      "",
      "module.exports = { quasarledger };",
      "",
    ].join("\n"),
  );
}

/// One bounded stdio MCP exchange: the whole request batch goes in, stdin
/// closes, and the server must answer every request and exit cleanly before
/// the timeout kills it (a broken server can never hang the run).
function mcpSession(executable, prefix, fixture, environment, requests) {
  const result = spawnSync(executable, [...prefix, "mcp", fixture], {
    cwd: temporary,
    env: environment,
    input: requests.map((request) => JSON.stringify(request)).join("\n") + "\n",
    encoding: "utf8",
    timeout: MCP_TIMEOUT_MS,
    maxBuffer: MCP_MAX_BUFFER,
  });
  assert.equal(
    result.status,
    0,
    `MCP session did not shut down cleanly after stdin EOF (status ${result.status}): ${result.stderr}`,
  );
  const byId = {};
  for (const line of result.stdout.trim().split("\n")) {
    const message = JSON.parse(line);
    if (message.id !== null && message.id !== undefined)
      byId[message.id] = message;
  }
  for (const request of requests) {
    if (request.id !== undefined)
      assert.ok(byId[request.id], `missing MCP response for id ${request.id}`);
  }
  return byId;
}

const initializeRequest = (id) => ({
  jsonrpc: "2.0",
  id,
  method: "initialize",
  params: {
    protocolVersion: PROTOCOL_VERSION,
    capabilities: {},
    clientInfo: { name: "release-smoke", version: "0" },
  },
});

/// Runs the identical offline docs+code fixture against one installed form
/// (npm launcher or extracted native binary), outside the source checkout,
/// with the generated cache isolated under the temporary tree.
function functionalSmoke(label, executable, prefix, extraEnv) {
  const fixture = join(temporary, `fixture ${label}`);
  const cache = join(temporary, `cache ${label}`);
  mkdirSync(fixture, { recursive: true });
  writeFixture(fixture);
  const environment = { ...process.env, ...extraEnv, REPOISE_CACHE_DIR: cache };
  const cli = (args) => {
    const result = spawnSync(executable, [...prefix, ...args, fixture], {
      cwd: temporary,
      env: environment,
      encoding: "utf8",
      timeout: CLI_TIMEOUT_MS,
    });
    assert.equal(
      result.status,
      0,
      `${label} repoise ${args.join(" ")} failed (status ${result.status}): ${result.stderr}`,
    );
    return result.stdout;
  };
  cli(["init", "--preset", "docs-code-lexical"]);
  assert.match(
    readFileSync(join(fixture, "repoise.config.json"), "utf8"),
    /docs-code-lexical/,
  );
  cli(["index"]);
  const search = JSON.parse(cli(["search", "--json", "--query", QUERY]));
  const paths = search.results.map((hit) => hit.path);
  for (const expected of [DOCS_PATH, CODE_TS_PATH, CODE_JS_PATH]) {
    assert.ok(
      paths.includes(expected),
      `${label} search missed ${expected}: ${paths.join(", ")}`,
    );
  }
  const docsHit = search.results.find((hit) => hit.path === DOCS_PATH);
  const codeHit = search.results.find((hit) => hit.path === CODE_TS_PATH);
  const docsRead = JSON.parse(
    cli(["read", "--json", "--source-id", docsHit.source_id]),
  );
  assert.equal(docsRead.path, DOCS_PATH);
  assert.match(
    docsRead.text,
    /quasarledger service reconciles offline corpus records/,
  );
  const codeRead = JSON.parse(
    cli(["read", "--json", "--source-id", codeHit.source_id]),
  );
  assert.match(codeRead.text, /function quasarledger/);
  const status = JSON.parse(cli(["status", "--json"]));
  assert.equal(status.freshness.status, "fresh");
  const check = JSON.parse(cli(["check", "--json", "--fresh"]));
  assert.equal(check.category, "ok");
  // stdio MCP: initialization, initialized notification, tool discovery,
  // search and status over the same shared services as the CLI.
  const found = mcpSession(executable, prefix, fixture, environment, [
    initializeRequest(1),
    { jsonrpc: "2.0", method: "notifications/initialized" },
    { jsonrpc: "2.0", id: 2, method: "tools/list", params: {} },
    {
      jsonrpc: "2.0",
      id: 3,
      method: "tools/call",
      params: { name: "search_project_knowledge", arguments: { query: QUERY } },
    },
    {
      jsonrpc: "2.0",
      id: 4,
      method: "tools/call",
      params: { name: "project_knowledge_status", arguments: {} },
    },
  ]);
  assert.equal(found[1].result.protocolVersion, PROTOCOL_VERSION);
  assert.equal(found[1].result.serverInfo.name, "repoise");
  assert.deepEqual(
    found[2].result.tools.map((tool) => tool.name),
    [
      "search_project_knowledge",
      "read_project_knowledge",
      "related_project_knowledge",
      "project_knowledge_status",
    ],
  );
  const mcpSearch = JSON.parse(found[3].result.content[0].text);
  assert.deepEqual(
    mcpSearch,
    search,
    "CLI and MCP search results must be identical",
  );
  const mcpStatus = JSON.parse(found[4].result.content[0].text);
  assert.equal(mcpStatus.freshness.status, "fresh");
  // Exact read through MCP, then a refresh attempt the read-only server must
  // refuse (the tool is not advertised and the call is denied).
  const denied = mcpSession(executable, prefix, fixture, environment, [
    initializeRequest(1),
    { jsonrpc: "2.0", method: "notifications/initialized" },
    {
      jsonrpc: "2.0",
      id: 2,
      method: "tools/call",
      params: {
        name: "read_project_knowledge",
        arguments: { sourceId: docsHit.source_id },
      },
    },
    {
      jsonrpc: "2.0",
      id: 3,
      method: "tools/call",
      params: { name: "refresh_project_knowledge", arguments: {} },
    },
  ]);
  const mcpRead = JSON.parse(denied[2].result.content[0].text);
  assert.deepEqual(
    mcpRead,
    docsRead,
    "CLI and MCP read results must be identical",
  );
  assert.equal(denied[3].result.isError, true);
  assert.match(denied[3].result.content[0].text, /read-only/);
  // The cache resolved under the isolated override, not the fixture root.
  assert.ok(existsSync(cache), `${label} cache directory was not created`);
  assert.ok(!existsSync(join(fixture, ".repoise")));
}

/// Bounded helpers for driving a long-lived `watch --events` child.
function withTimeout(promise, ms, label) {
  return Promise.race([
    promise,
    new Promise((_, reject) =>
      setTimeout(
        () => reject(new Error(`${label} timed out after ${ms} ms`)),
        ms,
      ).unref(),
    ),
  ]);
}

/// Reads every stdout line of the watch child into a queue as it arrives, so
/// no line (including those that arrive while another await is pending) is
/// lost, and `next` resolves on the next line, on `null` at EOF, or throws
/// on a timeout, spawn error or stream close.
function watchLineFeed(reader, child, overallMs, label) {
  const queue = [];
  const wakeups = new Set();
  let eof = false;
  let failure = null;
  const wake = () => {
    for (const wakeup of [...wakeups]) wakeup();
  };
  const onLine = (value) => {
    queue.push(value);
    wake();
  };
  const onEof = () => {
    eof = true;
    wake();
  };
  const onError = (error) => {
    failure =
      failure ?? new Error(`${label}: the process failed: ${error.message}`);
    wake();
  };
  const timer = setTimeout(() => {
    failure = failure ?? new Error(`${label} timed out after ${overallMs} ms`);
    wake();
  }, overallMs).unref();
  reader.on("line", onLine);
  reader.once("close", onEof);
  child.once("error", onError);
  const next = async (timeoutMs, what) => {
    const deadline = Date.now() + timeoutMs;
    for (;;) {
      if (queue.length > 0) return queue.shift();
      if (eof) return null;
      if (failure) throw failure;
      const remaining = deadline - Date.now();
      if (remaining <= 0)
        throw new Error(`${what} timed out after ${timeoutMs} ms`);
      // Bounded poll: the unref'd timers never keep the loop alive; the
      // ref'd child and its open stdout pipe do.
      await new Promise((resolve) => {
        const wakeup = () => resolve();
        wakeups.add(wakeup);
        setTimeout(
          () => {
            wakeups.delete(wakeup);
            resolve();
          },
          Math.min(remaining, 100),
        ).unref();
      });
    }
  };
  const close = () => {
    clearTimeout(timer);
    reader.off("line", onLine);
    reader.off("close", onEof);
    child.off("error", onError);
    wake();
    wakeups.clear();
  };
  return { next, close };
}

/// Normalize a path for cross-platform comparison (the smoke runs on
/// Linux/macOS/Windows and compares Node-side paths against the CLI's).
function normalizePath(path) {
  return path.replace(/\\/g, "/").replace(/\/+/g, "/");
}

function streamEvents(stdout) {
  return stdout
    .trim()
    .split("\n")
    .filter((line) => line.trim() !== "")
    .map((line) => JSON.parse(line));
}

/// The AP-07 first-slice machine contract against one installed form,
/// reusing the fixture functionalSmoke already indexed: the capabilities
/// handshake (version is identity; capability negotiation is the authority),
/// a real `index --events` stream, scope ownership while `watch` holds the
/// lock, a refused second writer, and bounded cancellation. See
/// docs/automation.md ("AP-07 first-slice contract").
async function ap07ContractSmoke(label, executable, prefix, extraEnv) {
  const fixture = join(temporary, `fixture ${label}`);
  const cache = join(temporary, `cache ${label}`);
  const environment = { ...process.env, ...extraEnv, REPOISE_CACHE_DIR: cache };
  const rootCanonical = realpathSync(fixture);
  const streamOptions = { operation: "index" };
  const cli = (args) => {
    const result = spawnSync(executable, [...prefix, ...args, fixture], {
      cwd: temporary,
      env: environment,
      encoding: "utf8",
      timeout: CLI_TIMEOUT_MS,
    });
    assert.equal(
      result.status,
      0,
      `${label} repoise ${args.join(" ")} failed (status ${result.status}): ${result.stderr}`,
    );
    return JSON.parse(result.stdout);
  };

  // 1. The installed CLI reports the candidate version and passes the
  // minimum AP-07 capability contract.
  const cap = spawnSync(executable, [...prefix, "capabilities", "--json"], {
    cwd: temporary,
    env: environment,
    encoding: "utf8",
    timeout: CLI_TIMEOUT_MS,
  });
  assert.equal(
    cap.status,
    0,
    `${label} capabilities failed (status ${cap.status}): ${cap.stderr}`,
  );
  const doc = JSON.parse(cap.stdout);
  assert.equal(
    doc.version,
    manifest().version,
    `${label} installed CLI reports version ${doc.version}`,
  );
  const verdict = checkAp07Handshake(doc);
  assert.deepEqual(
    verdict,
    { ok: true, version: manifest().version },
    `${label} capabilities rejected: ${verdict.reasons?.join("; ") ?? verdict}`,
  );

  // 2. A real `index --events` run streams a valid v1 envelope for this
  // scope and ends with exactly one terminal event.
  const index = spawnSync(
    executable,
    [...prefix, "index", "--events", fixture],
    {
      cwd: temporary,
      env: environment,
      encoding: "utf8",
      timeout: CLI_TIMEOUT_MS,
    },
  );
  assert.equal(
    index.status,
    0,
    `${label} index --events failed (status ${index.status}): ${index.stderr}`,
  );
  const indexEvents = streamEvents(index.stdout);
  const indexVerdict = validateEventStream(indexEvents, streamOptions);
  assert.deepEqual(
    indexVerdict,
    { ok: true },
    `${label} index stream invalid: ${JSON.stringify(indexVerdict.problems)}`,
  );
  const started = indexEvents[0];
  assert.equal(
    normalizePath(started.payload.scope.root),
    normalizePath(rootCanonical),
    `${label} index stream reports the wrong root: ${started.payload.scope.root}`,
  );
  const completed = indexEvents.at(-1);
  assert.equal(completed.type, "completed");
  assert.ok(
    Number.isInteger(completed.payload.result.files_indexed) &&
      completed.payload.result.files_indexed >= 3,
    `${label} the fixture files were not indexed: ${JSON.stringify(
      completed.payload.result,
    )}`,
  );

  // 3. `watch` owns the scope; `status` reports the authoritative writer.
  const child = spawn(
    executable,
    [...prefix, "watch", "--events", "--interval", "200", fixture],
    {
      cwd: temporary,
      env: environment,
      stdio: ["ignore", "pipe", "ignore"],
    },
  );
  // A spawned child does not keep the Node event loop alive on its own while
  // every timer here is unref'd; ref it so the bounded awaits can complete.
  child.ref();
  const reader = readline.createInterface({ input: child.stdout });
  const feed = watchLineFeed(reader, child, WATCH_TIMEOUT_MS, `${label} watch`);
  const exitCode = new Promise((resolveExit) =>
    child.on("exit", (code) => resolveExit(code)),
  );
  const drainFeed = async (timeoutMs, what) => {
    const rest = [];
    for (;;) {
      const line = await feed.next(timeoutMs, what);
      if (line === null) break;
      rest.push(line);
    }
    return rest;
  };
  try {
    const startedLine = await feed.next(
      WATCH_TIMEOUT_MS,
      `${label} watch started`,
    );
    assert.ok(
      startedLine !== null,
      `${label}: watch closed its output before the started event`,
    );
    const watchStarted = JSON.parse(startedLine);
    assert.equal(watchStarted.type, "started");
    assert.equal(watchStarted.payload.operation, "watch");
    assert.equal(
      normalizePath(watchStarted.payload.scope.root),
      normalizePath(rootCanonical),
      `${label} watch stream reports the wrong root`,
    );

    const deadline = Date.now() + WATCH_TIMEOUT_MS;
    let owned;
    for (;;) {
      const probe = cli(["status", "--json"]);
      owned = probe;
      if (
        probe.writer.lock_held === true &&
        probe.writer.state === "owned" &&
        probe.owner?.state === "active" &&
        probe.owner?.operation === "watch"
      )
        break;
      assert.ok(
        Date.now() < deadline,
        `${label} the scope never became owned: ${JSON.stringify(probe)}`,
      );
      await new Promise((resolveSleep) => setTimeout(resolveSleep, 100));
    }

    // 4. A second writer for the owned scope is refused with a stable
    // machine failure before any work starts.
    const busy = spawnSync(
      executable,
      [...prefix, "index", "--events", fixture],
      {
        cwd: temporary,
        env: environment,
        encoding: "utf8",
        timeout: CLI_TIMEOUT_MS,
      },
    );
    assert.equal(
      busy.status,
      1,
      `${label} a second writer must be refused (status ${busy.status}): ${busy.stderr}`,
    );
    const busyEvents = streamEvents(busy.stdout);
    const busyVerdict = validateEventStream(busyEvents, streamOptions);
    assert.deepEqual(
      busyVerdict,
      { ok: true },
      `${label} busy stream invalid: ${JSON.stringify(busyVerdict.problems)}`,
    );
    const failed = busyEvents.at(-1);
    assert.equal(failed.type, "failed");
    assert.equal(failed.payload.code, "operational");
    assert.equal(failed.payload.reason, "scope-owned");
    assert.equal(failed.payload.retryable, true);

    // 5. Bounded cancellation: graceful SIGINT on Unix; on Windows
    // termination is abrupt, so the stream must end incomplete (no
    // terminal event), never with a fake completion.
    if (process.platform === "win32") {
      child.kill("SIGTERM");
      await withTimeout(exitCode, WATCH_TIMEOUT_MS, `${label} watch exit`);
      const rest = await drainFeed(
        WATCH_TIMEOUT_MS,
        `${label} watch stream close`,
      );
      const events = [watchStarted, ...rest.map((line) => JSON.parse(line))];
      const verdict2 = validateEventStream(events, { operation: "watch" });
      assert.equal(
        verdict2.ok,
        false,
        `${label} an abrupt Windows kill must leave an incomplete stream`,
      );
      assert.ok(
        verdict2.problems.some((problem) => problem.includes("terminal event")),
        `${label} expected the incomplete-stream problem: ${verdict2.problems}`,
      );
    } else {
      process.kill(child.pid, "SIGINT");
      const code = await withTimeout(
        exitCode,
        WATCH_TIMEOUT_MS,
        `${label} watch exit`,
      );
      assert.equal(
        code,
        0,
        `${label} SIGINT must stop watch with exit code 0 (got ${code})`,
      );
      const rest = await drainFeed(
        WATCH_TIMEOUT_MS,
        `${label} watch stream close`,
      );
      const events = [watchStarted, ...rest.map((line) => JSON.parse(line))];
      const verdict2 = validateEventStream(events, { operation: "watch" });
      assert.deepEqual(
        verdict2,
        { ok: true },
        `${label} cancellation stream invalid: ${JSON.stringify(
          verdict2.problems,
        )}`,
      );
      assert.equal(events.at(-1).type, "cancelled");
      assert.equal(events.at(-1).payload.reason, "signal");
    }

    // 6. The OS released the lock on exit: the scope is free and the
    // descriptive marker is gone.
    const deadline2 = Date.now() + WATCH_TIMEOUT_MS;
    let cleared;
    for (;;) {
      cleared = cli(["status", "--json"]);
      if (cleared.writer.state === "free" && cleared.owner == null) break;
      assert.ok(
        Date.now() < deadline2,
        `${label} the scope never returned to free: ${JSON.stringify(cleared)}`,
      );
      await new Promise((resolveSleep) => setTimeout(resolveSleep, 100));
    }
  } finally {
    feed.close();
    reader.close();
    if (!child.killed) child.kill("SIGKILL");
  }
}

const temporary = mkdtempSync(join(tmpdir(), "repoise smoke "));
try {
  const consumer = join(temporary, "consumer");
  mkdirSync(consumer);
  writeFileSync(
    join(consumer, "package.json"),
    JSON.stringify({ name: "release-consumer", private: true }),
  );
  npm([
    "pack",
    join(root, "npm/repoise"),
    "--json",
    "--ignore-scripts",
    "--pack-destination",
    temporary,
  ]);
  const wrapper = join(temporary, `repoise-${manifest().version}.tgz`);
  const native = join(directory, `${spec.package}-${manifest().version}.tgz`);
  npm(
    [
      "install",
      "--ignore-scripts",
      "--no-audit",
      "--no-fund",
      "--offline",
      "--omit=optional",
      native,
      wrapper,
    ],
    { cwd: consumer },
  );
  const launch = join(consumer, "node_modules/repoise/bin/repoise.cjs");
  assert.equal(
    run(process.execPath, [launch, "--version"], { cwd: consumer }),
    `repoise ${manifest().version}`,
  );
  assert.match(
    run(process.execPath, [launch, "--help"], { cwd: consumer }),
    /USAGE:\s+repoise/,
  );
  const invalid = spawnSync(process.execPath, [launch, "--unknown"], {
    cwd: consumer,
    encoding: "utf8",
  });
  assert.equal(invalid.status, 2);
  assert.equal(invalid.stdout, "");
  assert.match(invalid.stderr, /--help/);
  // The versioned config schema ships with the installed launcher.
  const schema = readFileSync(join(root, SCHEMA_FILE), "utf8");
  assert.equal(
    readFileSync(join(consumer, "node_modules/repoise", SCHEMA_FILE), "utf8"),
    schema,
  );
  // The machine-contract schemas ship unmodified with the installed launcher.
  for (const machineSchema of MACHINE_SCHEMA_FILES)
    assert.equal(
      readFileSync(
        join(consumer, "node_modules/repoise", "schemas", machineSchema),
        "utf8",
      ),
      readFileSync(join(root, "schemas", machineSchema), "utf8"),
      `the launcher ships a modified ${machineSchema}`,
    );
  functionalSmoke("launcher", process.execPath, [launch], {});
  await ap07ContractSmoke("launcher", process.execPath, [launch], {});
  const unpacked = join(temporary, "archive");
  mkdirSync(unpacked);
  const archive = readdirSync(directory).find((name) =>
    name.endsWith(".tar.gz"),
  );
  run("tar", ["-xzf", join(directory, archive), "-C", unpacked]);
  const executable = join(unpacked, spec.binary);
  const nativeResult = spawnSync(executable, ["--version"], {
    cwd: consumer,
    env: { ...process.env, PATH: "" },
    encoding: "utf8",
  });
  assert.equal(nativeResult.status, 0, nativeResult.stderr);
  assert.equal(nativeResult.stdout.trim(), `repoise ${manifest().version}`);
  assert.equal(
    readFileSync(join(unpacked, "LICENSE"), "utf8"),
    readFileSync(join(root, "LICENSE"), "utf8"),
  );
  // The versioned config schema ships with the native archive too.
  assert.equal(readFileSync(join(unpacked, SCHEMA_FILE), "utf8"), schema);
  // The machine-contract schemas ship unmodified with the native archive.
  for (const machineSchema of MACHINE_SCHEMA_FILES)
    assert.equal(
      readFileSync(join(unpacked, "schemas", machineSchema), "utf8"),
      readFileSync(join(root, "schemas", machineSchema), "utf8"),
      `the native archive ships a modified ${machineSchema}`,
    );
  // Full functional operation with no Node/npm on PATH.
  functionalSmoke("native", executable, [], { PATH: "" });
  await ap07ContractSmoke("native", executable, [], { PATH: "" });
  // Old/incompatible handshakes are rejected with actionable upgrade
  // guidance; additive unknown fields stay tolerated (capability
  // negotiation, not version strings, decides compatibility).
  for (const [name, expectedOk] of [
    ["compatible", true],
    ["additive", true],
    ["legacyAlpha1", false],
    ["futureEventStream", false],
    ["versionOnly", false],
  ]) {
    const fixtureVerdict = checkAp07Handshake(handshakeFixtures[name]);
    assert.equal(
      fixtureVerdict.ok,
      expectedOk,
      `fixture "${name}" must ${expectedOk ? "pass" : "fail"} the contract`,
    );
    if (!expectedOk)
      assert.match(fixtureVerdict.guidance, /Upgrade to Repoise/);
  }
  console.log(
    "Packed launcher and native archive smoke passed (offline consumer, no install scripts, offline docs+code fixture, MCP read-only, no Node on native PATH, AP-07 capabilities/events/ownership contract).",
  );
} finally {
  rmSync(temporary, { recursive: true, force: true });
}
