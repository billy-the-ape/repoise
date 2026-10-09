import assert from "node:assert/strict";
import {
  mkdtempSync,
  mkdirSync,
  writeFileSync,
  rmSync,
  readdirSync,
  readFileSync,
  existsSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { spawnSync } from "node:child_process";
import { root, targets, manifest, npm, run } from "./common.mjs";

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
  functionalSmoke("launcher", process.execPath, [launch], {});
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
  // Full functional operation with no Node/npm on PATH.
  functionalSmoke("native", executable, [], { PATH: "" });
  console.log(
    "Packed launcher and native archive smoke passed (offline consumer, no install scripts, offline docs+code fixture, MCP read-only, no Node on native PATH).",
  );
} finally {
  rmSync(temporary, { recursive: true, force: true });
}
