// Repoise offline lexical evaluation harness (card K6, item 6).
//
// Rebuilds the frozen synthetic fixtures (fixtures/manifest.json) as
// deterministic Git workspaces, runs the native CLI (`repoise index`,
// `status`, `check --fresh`, `search`) under the docs-only and
// docs-code-lexical presets, validates provenance of every returned hit
// (content hash at the snapshot revision, line bounds, history revisions),
// and reports evidence recall@5, exact-lookup success, policy failures
// (cross-scope / stale-text / secret), cold build and warm search timings,
// corpus/index sizes, hardware, versions and declared settings.
//
// Output (kept out of Git): <out>/results.json (machine-readable) and
// <out>/report.md (concise, measured/failed/unmeasured gates).
//
// Usage:
//   node scripts/eval/harness.mjs [--out DIR] [--iterations N]
//                                 [--binary PATH] [--replay-evidence FILE]
//
// No network, provider credentials or private content are used: hybrid
// presets and the real agent pilot are explicitly unmeasured here.

import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import {
  cpSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  statSync,
  writeFileSync,
} from "node:fs";
import {
  cpus,
  freemem,
  hostname,
  platform,
  release,
  totalmem,
  tmpdir,
} from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { summarizeReplayEvidence, validateReplayEvidence } from "./replay.mjs";

const HERE = dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = resolve(HERE, "..", "..");
const FIXTURES = join(HERE, "fixtures");
const CLI_TIMEOUT_MS = 120_000;

const PRESET_RUNS = [
  { corpus: "acme", preset: "docs-code-lexical" },
  { corpus: "acme", preset: "docs-only" },
  { corpus: "docs-site", preset: "docs-only" },
  { corpus: "pyrepo", preset: "docs-code-lexical" },
];

const PRESET_PROBES = [
  "computeBatchId",
  "reconcileLedger",
  "legacyShimV2 items",
];
const SECRET_MARKER = "REPOISE-FAKE-SECRET-0001";

// ---------------------------------------------------------------------------
// Pure metric helpers (exported for the test suite).

/// Nearest-rank percentile over a (non-empty) sample; p in [0, 100].
export function percentile(sample, p) {
  if (sample.length === 0) return null;
  const sorted = [...sample].sort((a, b) => a - b);
  const rank = Math.max(1, Math.ceil((p / 100) * sorted.length));
  return sorted[rank - 1];
}

/// True when any gold (path + inclusive line range) overlaps any top hit.
export function evidenceInTop5(hits, gold) {
  return gold.some((g) =>
    hits.some(
      (h) =>
        h.path === g.path &&
        h.line_start <= g.line_end &&
        h.line_end >= g.line_start,
    ),
  );
}

/// True when a history-lane gold (commit ref + affected path) is satisfied by
/// any top hit.
export function historyEvidenceInTop5(hits, gold, revisions) {
  return gold.some((g) =>
    hits.some(
      (h) =>
        revisions[g.revisionRef] !== undefined &&
        h.revision_id === `git:${revisions[g.revisionRef]}` &&
        h.affected_paths.includes(g.path),
    ),
  );
}

/// Evaluates the master-plan section 11 gates. Measured gates are judged from
/// harness metrics; gates needing operator-configured hybrid operation or real
/// agent replay evidence are always "unmeasured" here (never fabricated).
export function evaluateGates(metrics, replaySummary) {
  const gates = [];
  const exactOk =
    metrics.exact.total > 0 &&
    metrics.exact.passed === metrics.exact.total &&
    metrics.provenance.violations.length === 0;
  gates.push({
    id: "G1-exact-lookup",
    name: "100% exact path/symbol fixture lookup with valid source/range provenance",
    target: "100%",
    status: exactOk ? "measured-passed" : "measured-failed",
    measured: `${metrics.exact.passed}/${metrics.exact.total} exact queries; ${metrics.provenance.validatedHits} hits provenance-validated; ${metrics.provenance.violations.length} violations`,
  });
  const isolationViolations = metrics.isolation?.violations.length ?? 0;
  const policyFailures =
    metrics.policy.secretLeaks.length +
    metrics.policy.crossScopeLeaks.length +
    metrics.policy.staleLeaks.length +
    metrics.policy.mustNotViolations.length +
    isolationViolations;
  gates.push({
    id: "G2-policy",
    name: "Zero cross-scope / stale-text / secret-policy fixture leaks, including shared-cache scope isolation and stale exact reads",
    target: "0 leaks",
    status: policyFailures === 0 ? "measured-passed" : "measured-failed",
    measured: `${policyFailures} leaks (secret ${metrics.policy.secretLeaks.length}, cross-scope ${metrics.policy.crossScopeLeaks.length}, stale ${metrics.policy.staleLeaks.length}, must-not ${metrics.policy.mustNotViolations.length}, shared-cache isolation ${isolationViolations})`,
  });
  gates.push({
    id: "G3-recall",
    name: "At least 85% evidence recall@5 on held-out eligible conceptual queries",
    target: ">= 85%",
    status:
      metrics.recall.eligible > 0 && metrics.recall.recallAt5 >= 0.85
        ? "measured-passed"
        : "measured-failed",
    measured: `${metrics.recall.found}/${metrics.recall.eligible} held-out conceptual queries (${(metrics.recall.recallAt5 * 100).toFixed(1)}%)`,
  });
  const warm = metrics.warmP95Ms;
  gates.push({
    id: "G4-warm-search",
    name: "p95 warm lexical search <= 500 ms (per-CLI-invocation wall time)",
    target: "p95 <= 500 ms",
    status:
      warm !== null && warm <= 500 ? "measured-passed" : "measured-failed",
    measured:
      warm === null
        ? "no warm samples"
        : `p95 ${warm} ms (declared hardware in report)`,
  });
  gates.push({
    id: "G5-token-reduction",
    name: "At least 20% median model input token reduction vs curated docs+rg, no task-correctness decrease",
    target: ">= 20% with equal correctness",
    status: "unmeasured",
    measured: replaySummary
      ? "replay evidence imported; the mean-based reduction is descriptive only and does not establish the median gate — unmeasured until the owner-approved live pilot provides measured medians"
      : "requires real agent replay evidence (import format: scripts/eval/replay-evidence.schema.json); no numbers are inferred or fabricated",
  });
  gates.push({
    id: "G6-code-vectors",
    name: "Code vectors improve code-query recall@5 by >= 10 percentage points over code lexical at acceptable cost",
    target: ">= +10 points",
    status: "unmeasured",
    measured:
      "requires operator-configured embedding provider; hybrid is explicitly unrun by this harness",
  });
  gates.push({
    id: "G7-hybrid-latency",
    name: "p95 warm hybrid search <= 2 s (excluding remote embedding request)",
    target: "p95 <= 2 s",
    status: "unmeasured",
    measured:
      "requires operator-configured embedding provider; hybrid is explicitly unrun by this harness",
  });
  return gates;
}

// ---------------------------------------------------------------------------
// Fixture reconstruction.

function loadJson(path) {
  return JSON.parse(readFileSync(path, "utf8"));
}

function sha256File(path) {
  return createHash("sha256").update(readFileSync(path)).digest("hex");
}

/// Runs one git command with a bounded timeout; throws on failure.
function git(cwd, args, extraEnv = {}) {
  const result = spawnSync("git", args, {
    cwd,
    env: { ...process.env, ...extraEnv },
    encoding: "utf8",
    timeout: CLI_TIMEOUT_MS,
  });
  if (result.status !== 0) {
    throw new Error(
      `git ${args.join(" ")} failed: ${(result.stderr.trim() || result.stdout.trim()).slice(0, 400)}`,
    );
  }
  return result.stdout.trim();
}

/// Rebuilds one frozen corpus as a deterministic Git workspace. Returns
/// { dir, revisions (ref -> sha), headSha } and verifies every copied file
/// against the frozen manifest hashes.
export function buildWorkspace(corpusName, manifest) {
  const dir = mkdtempSync(join(tmpdir(), `repoise-eval-${corpusName}-`));
  const corpus = manifest.corpora[corpusName];
  for (const [key, frozen] of Object.entries(manifest.files)) {
    if (!key.startsWith(`${corpusName}/`)) continue;
    const rel = key.slice(corpusName.length + 1);
    const dest = join(dir, rel);
    mkdirSync(dirname(dest), { recursive: true });
    cpSync(join(FIXTURES, key), dest);
    const actual = sha256File(dest);
    if (actual !== frozen) {
      throw new Error(
        `fixture ${key} changed since the manifest freeze (${actual} != ${frozen})`,
      );
    }
  }
  git(dir, ["init", "-b", manifest.git.branch]);
  const revisions = {};
  for (const commit of corpus.commits) {
    for (const rel of commit.files) {
      const dest = join(dir, rel);
      mkdirSync(dirname(dest), { recursive: true });
      if (commit.fileContents?.[rel] !== undefined) {
        writeFileSync(dest, commit.fileContents[rel]);
      } else {
        const source = join(FIXTURES, `${corpusName}/${rel}`);
        if (!existsSync(source)) {
          throw new Error(
            `commit ${commit.ref} references missing fixture file ${rel}`,
          );
        }
        cpSync(source, dest);
      }
      git(dir, ["add", "--", rel]);
    }
    const env = {
      GIT_AUTHOR_DATE: commit.date,
      GIT_COMMITTER_DATE: commit.date,
    };
    git(
      dir,
      [
        "-c",
        `user.name=${manifest.git.userName}`,
        "-c",
        `user.email=${manifest.git.userEmail}`,
        "commit",
        "-m",
        commit.message,
      ],
      env,
    );
    revisions[commit.ref] = git(dir, ["rev-parse", "HEAD"]);
  }
  const headSha = revisions[corpus.commits.at(-1).ref];
  return { dir, revisions, headSha };
}

/// Applies the corpus's declared dirty (uncommitted) worktree operations.
export function applyDirty(corpusName, workspace, manifest) {
  const corpus = manifest.corpora[corpusName];
  for (const op of corpus.dirty ?? []) {
    const target = join(workspace.dir, op.path);
    const current = readFileSync(target, "utf8");
    if (op.op === "appendLines") {
      writeFileSync(
        target,
        current.replace(/\n?$/, "\n") + op.lines.join("\n") + "\n",
      );
    } else if (op.op === "replaceText") {
      if (!current.includes(op.old))
        throw new Error(`dirty replace: "${op.old}" not found in ${op.path}`);
      writeFileSync(target, current.replace(op.old, op.new));
    } else {
      throw new Error(`unknown dirty op: ${op.op}`);
    }
  }
}

/// Runs the native CLI with a bounded timeout; returns { code, stdout, stderr, ms }.
export function runCli(binary, args, cwd, env) {
  const start = process.hrtime.bigint();
  const result = spawnSync(binary, args, {
    cwd,
    env,
    encoding: "utf8",
    timeout: CLI_TIMEOUT_MS,
    maxBuffer: 16 * 1024 * 1024,
  });
  const ms = Number(process.hrtime.bigint() - start) / 1_000_000;
  if (result.error) {
    throw new Error(`cli ${args.join(" ")} failed: ${result.error.message}`);
  }
  return {
    code: result.status,
    stdout: result.stdout,
    stderr: result.stderr,
    ms,
  };
}

function parseCliJson(stdout) {
  const text = stdout.trim();
  if (text === "") throw new Error("empty CLI output");
  return JSON.parse(text);
}

// ---------------------------------------------------------------------------
// Provenance validation.

/// Validates every chunk-lane hit against the snapshot revision: the path
/// must exist at that revision, the hit's content hash must equal the
/// SHA-256 of the file bytes at that revision, and the line range must be
/// inside the file. Returns { validated, violations }.
function validateSearchHits(response, workspace, snapshotSha) {
  let validated = 0;
  const violations = [];
  const cache = new Map();
  const contentAt = (path) => {
    if (!cache.has(path)) {
      const result = spawnSync("git", ["show", `${snapshotSha}:${path}`], {
        cwd: workspace.dir,
        encoding: "utf8",
        maxBuffer: 16 * 1024 * 1024,
      });
      cache.set(path, result.status === 0 ? result.stdout : null);
    }
    return cache.get(path);
  };
  for (const hit of response.results) {
    const content = contentAt(hit.path);
    if (content === null) {
      violations.push(
        `hit for ${hit.path}: path absent at snapshot revision ${snapshotSha.slice(0, 12)}`,
      );
      continue;
    }
    validated += 1;
    const expected = createHash("sha256").update(content).digest("hex");
    if (hit.revision_hash !== expected) {
      violations.push(
        `hit for ${hit.path}: revision_hash ${hit.revision_hash.slice(0, 12)} does not match snapshot content hash ${expected.slice(0, 12)}`,
      );
    }
    const lineCount =
      content.length > 0 ? content.replace(/\n$/, "").split("\n").length : 0;
    if (
      hit.line_start < 1 ||
      hit.line_end < hit.line_start ||
      hit.line_end > lineCount
    ) {
      violations.push(
        `hit for ${hit.path}: line range ${hit.line_start}-${hit.line_end} outside file (${lineCount} lines)`,
      );
    }
  }
  return { validated, violations };
}

/// Validates every history-lane hit: the revision must be one of the replayed
/// commits and every affected path must exist at that revision.
function validateHistoryHits(response, workspace) {
  let validated = 0;
  const violations = [];
  const known = new Set(Object.values(workspace.revisions));
  for (const hit of response.results) {
    const sha = hit.revision_id.replace(/^git:/, "");
    if (!known.has(sha)) {
      violations.push(
        `history hit revision ${sha.slice(0, 12)} is not a replayed fixture commit`,
      );
      continue;
    }
    validated += 1;
    for (const path of hit.affected_paths) {
      const result = spawnSync("git", ["cat-file", "-e", `${sha}:${path}`], {
        cwd: workspace.dir,
      });
      if (result.status !== 0) {
        violations.push(
          `history hit ${sha.slice(0, 12)}: affected path ${path} absent at that revision`,
        );
      }
    }
  }
  return { validated, violations };
}

// ---------------------------------------------------------------------------
// Shared-cache isolation phase (scope isolation + stale exact reads).
//
// Indexes two corpora into ONE shared cache root, then verifies, inspecting
// returned content as well as metadata:
//   * foreign source ids (from another scope) are not readable and do not
//     leak the other scope's content, in both directions;
//   * an in-scope exact read returns the exact snapshot text;
//   * after the indexed file changes, the earlier source id is rejected as
//     stale and the newly written bytes are never served.
// Cases not exercised by the fixtures are reported as unmeasured.

const ISOLATION_UNMEASURED = [
  "symlink/permission-based cache escapes and cross-user cache sharing",
  "cross-worktree reads within the same repository",
  "concurrent generation swaps during a read",
  "MCP-tool-level scope authorization (covered by the MCP contract tests, not this fixture)",
];

/// Judges raw isolation CLI outcomes (pure; exported for the test suite).
export function judgeIsolation(checks) {
  const violations = [];
  for (const { label, result, foreignSentinel } of checks.foreignReads) {
    if (result.code === 0) {
      violations.push(`${label}: foreign source id was accepted (exit code 0)`);
    } else if (foreignSentinel && result.stdout.includes(foreignSentinel)) {
      violations.push(
        `${label}: rejection output leaked the other scope's content`,
      );
    }
  }
  const good = checks.goodRead;
  if (good.code !== 0) {
    violations.push(`in-scope exact read failed (exit code ${good.code})`);
  } else if (good.text !== checks.goodReadExpectedText) {
    violations.push(
      "in-scope exact read did not return the exact snapshot text",
    );
  }
  const stale = checks.staleRead;
  if (stale.code === 0) {
    violations.push("stale read was accepted after the indexed file changed");
  } else if (
    checks.dirtySentinel &&
    (stale.stdout.includes(checks.dirtySentinel) ||
      stale.stderr.includes(checks.dirtySentinel))
  ) {
    violations.push("stale read output leaked the newly written (dirty) bytes");
  }
  return { passed: violations.length === 0, violations };
}

/// Runs the isolation phase against the built CLI. Returns a record with the
/// raw outcomes, the judgement and the explicitly unmeasured cases.
export function runIsolationPhase(binary, manifest) {
  const cache = mkdtempSync(join(tmpdir(), "repoise-eval-isolation-"));
  const env = { ...process.env, REPOISE_CACHE_DIR: cache };
  const acme = buildWorkspace("acme", manifest);
  const docsSite = buildWorkspace("docs-site", manifest);
  // Distinctive strings: docs-site prose absent from acme, an acme code
  // identifier absent from docs-site, and a dirty sentinel written at test
  // time (never part of the frozen fixtures).
  const docsSiteSentinel = "steward before publication";
  const acmeSentinel = "computeBatchId";
  const dirtySentinel = "dirtysentinelheron uncommitted isolation change";
  const cli = (dir, args) => runCli(binary, args, dir, env);
  try {
    // One shared cache root: both corpora are indexed side by side.
    const indexA = cli(acme.dir, [
      "--json",
      "index",
      "--preset",
      "docs-code-lexical",
      acme.dir,
    ]);
    const indexB = cli(docsSite.dir, [
      "--json",
      "index",
      "--preset",
      "docs-only",
      docsSite.dir,
    ]);
    if (indexA.code !== 0 || indexB.code !== 0) {
      throw new Error(
        `isolation indexing failed (acme exit ${indexA.code}, docs-site exit ${indexB.code})`,
      );
    }
    const searchA = cli(acme.dir, [
      "--json",
      "search",
      "--query",
      "computeBatchId",
      "--max-results",
      "5",
      "--preset",
      "docs-code-lexical",
      acme.dir,
    ]);
    const searchB = cli(docsSite.dir, [
      "--json",
      "search",
      "--query",
      "meridian archive digests",
      "--max-results",
      "5",
      "--preset",
      "docs-only",
      docsSite.dir,
    ]);
    const hitA = JSON.parse(searchA.stdout).results[0];
    const hitB = JSON.parse(searchB.stdout).results[0];
    if (!hitA || !hitB)
      throw new Error("isolation: expected at least one search hit per corpus");
    const foreignReads = [
      {
        label: "docs-site source id read from the acme scope",
        result: cli(acme.dir, [
          "--json",
          "read",
          "--source-id",
          hitB.source_id,
          acme.dir,
        ]),
        foreignSentinel: docsSiteSentinel,
      },
      {
        label: "acme source id read from the docs-site scope",
        result: cli(docsSite.dir, [
          "--json",
          "read",
          "--source-id",
          hitA.source_id,
          docsSite.dir,
        ]),
        foreignSentinel: acmeSentinel,
      },
    ];
    // In-scope exact read of an acme docs chunk, then change the file and
    // re-read the earlier source id.
    const searchC = cli(acme.dir, [
      "--json",
      "search",
      "--query",
      "two-phase commit mirror confirms",
      "--max-results",
      "20",
      "--preset",
      "docs-code-lexical",
      acme.dir,
    ]);
    const target = JSON.parse(searchC.stdout).results.find(
      (hit) => hit.path === "docs/current/architecture.md",
    );
    if (!target)
      throw new Error("isolation: architecture.md chunk not found by search");
    const goodResult = cli(acme.dir, [
      "--json",
      "read",
      "--source-id",
      target.source_id,
      acme.dir,
    ]);
    const show = spawnSync(
      "git",
      ["show", `${acme.headSha}:docs/current/architecture.md`],
      {
        cwd: acme.dir,
        encoding: "utf8",
      },
    );
    if (show.status !== 0)
      throw new Error("isolation: cannot read snapshot file via git");
    const goodReadExpectedText = show.stdout
      .replace(/\n$/, "")
      .split("\n")
      .slice(target.line_start - 1, target.line_end)
      .join("\n");
    let goodText = null;
    if (goodResult.code === 0) {
      const parsed = JSON.parse(goodResult.stdout);
      goodText = parsed.text ?? parsed.read?.text ?? null;
    }
    const filePath = join(acme.dir, "docs/current/architecture.md");
    const current = readFileSync(filePath, "utf8");
    if (!current.includes("two-phase commit")) {
      throw new Error(
        "isolation: expected phrase missing from architecture.md",
      );
    }
    writeFileSync(
      filePath,
      current.replace("two-phase commit", `${dirtySentinel} two-phase commit`),
    );
    const staleResult = cli(acme.dir, [
      "--json",
      "read",
      "--source-id",
      target.source_id,
      acme.dir,
    ]);
    const checks = {
      foreignReads,
      goodRead: {
        code: goodResult.code,
        stdout: goodResult.stdout,
        text: goodText,
      },
      goodReadExpectedText,
      staleRead: {
        code: staleResult.code,
        stdout: staleResult.stdout,
        stderr: staleResult.stderr,
      },
      dirtySentinel,
    };
    const verdict = judgeIsolation(checks);
    return {
      passed: verdict.passed,
      violations: verdict.violations,
      unmeasured: ISOLATION_UNMEASURED,
    };
  } finally {
    rmSync(acme.dir, { recursive: true, force: true });
    rmSync(docsSite.dir, { recursive: true, force: true });
    rmSync(cache, { recursive: true, force: true });
  }
}

// ---------------------------------------------------------------------------
// One corpus x preset run.

function runOne(spec, manifest, queries, binary, env, iterations) {
  const { corpus, preset } = spec;
  const workspace = buildWorkspace(corpus, manifest);
  const run = {
    corpus,
    preset,
    root: workspace.dir,
    revisions: workspace.revisions,
    headSha: workspace.headSha,
    coldBuildMs: null,
    status: null,
    index: null,
    sourceBytes: 0,
    warmSearchMs: null,
    checkFreshExitCode: null,
    queries: [],
    presetProbes: null,
    dirtyPhase: null,
    provenance: { validatedHits: 0, violations: [] },
  };
  for (const key of Object.entries(manifest.files).filter(([k]) =>
    k.startsWith(`${corpus}/`),
  )) {
    run.sourceBytes += statSync(join(FIXTURES, key[0])).size;
  }
  const cli = (args) => runCli(binary, args, workspace.dir, env);

  const indexResult = cli([
    "--json",
    "index",
    "--preset",
    preset,
    workspace.dir,
  ]);
  if (indexResult.code !== 0) {
    throw new Error(
      `index failed for ${corpus}/${preset}: ${indexResult.stderr.trim()}`,
    );
  }
  run.coldBuildMs = Math.round(indexResult.ms);

  const status = parseCliJson(
    cli(["--json", "status", "--preset", preset, workspace.dir]).stdout,
  );
  run.status = {
    version: status.version,
    revision: status.snapshot?.revision ?? null,
    dirtyCount: status.snapshot?.dirty_count ?? 0,
    freshness: status.freshness?.status ?? null,
  };
  run.index = {
    files: status.index?.files ?? null,
    chunks: status.index?.chunks ?? null,
    vectors: status.index?.vectors ?? 0,
    dbBytes: status.cache?.db_bytes ?? 0,
    walBytes: status.cache?.wal_bytes ?? 0,
  };
  const snapshotSha = status.snapshot?.revision?.replace(/^git:/, "");

  const check = cli([
    "--json",
    "check",
    "--fresh",
    "--preset",
    preset,
    workspace.dir,
  ]);
  run.checkFreshExitCode = check.code;

  const warm = [];
  const runQuery = (query, times) => {
    // max-results 20 (the cap) exposes the full ranking; metrics use the top 5.
    const args = [
      "--json",
      "search",
      "--query",
      query.query,
      "--max-results",
      "20",
      "--preset",
      preset,
      workspace.dir,
    ];
    if (query.lane === "history") args.splice(4, 0, "--lane", "history");
    let first = null;
    for (let i = 0; i < times; i += 1) {
      const result = cli(args);
      if (result.code !== 0) {
        throw new Error(
          `search failed for ${query.id} (${corpus}/${preset}): ${result.stderr.trim()}`,
        );
      }
      warm.push(result.ms);
      if (first === null) first = parseCliJson(result.stdout);
    }
    return first;
  };

  for (const query of queries.filter(
    (q) => q.corpus === corpus && q.preset === preset && !q.dirtyPhase,
  )) {
    const response = runQuery(query, iterations);
    if (query.lane === "search") {
      const { validated, violations } = validateSearchHits(
        response,
        workspace,
        snapshotSha,
      );
      run.provenance.validatedHits += validated;
      run.provenance.violations.push(...violations);
    } else {
      const { validated, violations } = validateHistoryHits(
        response,
        workspace,
      );
      run.provenance.validatedHits += validated;
      run.provenance.violations.push(...violations);
    }
    run.queries.push({
      id: query.id,
      query: query.query,
      lane: query.lane,
      kind: query.kind,
      heldOut: query.heldOut,
      results: response.results,
    });
  }

  if (corpus === "acme" && preset === "docs-only") {
    // Fixed probe set for the preset comparison: code identifiers must not
    // surface as code hits under the docs-only preset.
    run.presetProbes = PRESET_PROBES.map((probe) => {
      const response = runQuery({ query: probe, lane: "search" }, 1);
      return {
        probe,
        totalHits: response.results.length,
        codeHits: response.results.filter((hit) => hit.role === "code").length,
      };
    });
  }

  if (
    corpus === "acme" &&
    preset === "docs-code-lexical" &&
    manifest.corpora[corpus].dirty.length > 0
  ) {
    applyDirty(corpus, workspace, manifest);
    const dirtyStatus = parseCliJson(
      cli(["--json", "status", "--preset", preset, workspace.dir]).stdout,
    );
    const dirtyCheck = cli([
      "--json",
      "check",
      "--fresh",
      "--preset",
      preset,
      workspace.dir,
    ]);
    run.dirtyPhase = {
      dirtyCount: dirtyStatus.freshness?.dirty_count ?? 0,
      freshness: dirtyStatus.freshness?.status ?? null,
      checkFreshExitCode: dirtyCheck.code,
      queries: [],
    };
    for (const query of queries.filter(
      (q) => q.corpus === corpus && q.preset === preset && q.dirtyPhase,
    )) {
      const response = runQuery(query, iterations);
      const { validated, violations } = validateSearchHits(
        response,
        workspace,
        snapshotSha,
      );
      run.provenance.validatedHits += validated;
      run.provenance.violations.push(...violations);
      run.dirtyPhase.queries.push({
        id: query.id,
        query: query.query,
        lane: query.lane,
        kind: query.kind,
        heldOut: query.heldOut,
        results: response.results,
      });
    }
  }

  warm.sort((a, b) => a - b);
  run.warmSamples = warm;
  run.warmSearchMs =
    warm.length === 0
      ? null
      : {
          samples: warm.length,
          p50: Math.round(percentile(warm, 50)),
          p95: Math.round(percentile(warm, 95)),
          mean: Math.round(warm.reduce((a, b) => a + b, 0) / warm.length),
          max: Math.round(warm.at(-1)),
        };
  rmSync(workspace.dir, { recursive: true, force: true });
  return run;
}

// ---------------------------------------------------------------------------
// Metric assembly.

function judgeQuery(entry, query, workspace) {
  const hits5 = entry.results.slice(0, 5);
  const reasons = [];
  let pass = true;
  const mustNot = query.mustNot ?? [];
  for (const hit of entry.results) {
    for (const banned of mustNot) {
      if (hit.path === banned || hit.path.startsWith(banned)) {
        pass = false;
        reasons.push(`must-not path ${banned} surfaced (hit ${hit.path})`);
      }
    }
  }
  if (query.kind === "policy" || query.kind === "diagnostic") {
    if (entry.results.length > 0) {
      pass = false;
      reasons.push(`expected zero hits, got ${entry.results.length}`);
    }
  } else if (query.kind === "exact" || query.kind === "conceptual") {
    const found =
      query.lane === "history"
        ? historyEvidenceInTop5(hits5, query.gold, workspace.revisions)
        : evidenceInTop5(hits5, query.gold);
    if (!found) {
      pass = false;
      reasons.push("gold evidence not in top 5");
    }
  }
  if (query.expectTopRole && entry.results[0]?.role !== query.expectTopRole) {
    pass = false;
    reasons.push(
      `top hit role ${entry.results[0]?.role ?? "(none)"} != expected ${query.expectTopRole}`,
    );
  }
  if (query.assertRolesPresent) {
    const roles = new Set(entry.results.map((hit) => hit.role));
    for (const role of query.assertRolesPresent) {
      if (!roles.has(role)) {
        pass = false;
        reasons.push(`expected a hit with role ${role} in the results`);
      }
    }
  }
  return { pass, reasons };
}

function assembleMetrics(runs, queries) {
  const byId = new Map(queries.map((q) => [q.id, q]));
  const exact = { total: 0, passed: 0, failed: [] };
  const recall = { eligible: 0, found: 0, queries: [] };
  const policy = {
    secretLeaks: [],
    crossScopeLeaks: [],
    staleLeaks: [],
    mustNotViolations: [],
  };
  const provenance = { validatedHits: 0, violations: [] };
  const roleFailed = [];
  const warmAll = [];

  for (const run of runs) {
    const workspace = { revisions: run.revisions };
    for (const group of [
      run.queries,
      ...(run.dirtyPhase ? [run.dirtyPhase.queries] : []),
    ]) {
      for (const entry of group) {
        const query = byId.get(entry.id);
        const verdict = judgeQuery(entry, query, workspace);
        entry.status = verdict.pass ? "pass" : "fail";
        entry.reasons = verdict.reasons;
        if (query.kind === "exact") {
          exact.total += 1;
          if (verdict.pass) exact.passed += 1;
          else exact.failed.push(entry.id);
        }
        if (query.heldOut && query.kind === "conceptual") {
          recall.eligible += 1;
          const foundHit =
            query.lane === "history"
              ? historyEvidenceInTop5(
                  entry.results.slice(0, 5),
                  query.gold,
                  workspace.revisions,
                )
              : evidenceInTop5(entry.results.slice(0, 5), query.gold);
          if (foundHit) recall.found += 1;
          recall.queries.push({ id: entry.id, found: foundHit });
        }
        if (query.category === "secret-policy" && entry.results.length > 0) {
          policy.secretLeaks.push(entry.id);
        }
        if (query.category === "cross-scope" && entry.results.length > 0) {
          policy.crossScopeLeaks.push(entry.id);
        }
        if (
          query.category === "dirty-stale" &&
          query.kind === "policy" &&
          entry.results.length > 0
        ) {
          policy.staleLeaks.push(entry.id);
        }
        for (const reason of verdict.reasons) {
          if (reason.startsWith("must-not"))
            policy.mustNotViolations.push(entry.id);
          if (
            reason.startsWith("top hit role") ||
            reason.startsWith("expected a hit with role")
          ) {
            roleFailed.push(entry.id);
          }
        }
        if (query.expectTopRole && !verdict.pass) roleFailed.push(entry.id);
      }
    }
    provenance.validatedHits += run.provenance.validatedHits;
    provenance.violations.push(
      ...run.provenance.violations.map(
        (v) => `[${run.corpus}/${run.preset}] ${v}`,
      ),
    );
    // Stale-text leak: any hit whose content hash is not the snapshot hash is
    // already a provenance violation; the requireSnapshotHash queries add an
    // explicit dirty-bytes check below.
    warmAll.push(...(run.warmSamples ?? []));
  }
  // Explicit stale-text check for requireSnapshotHash queries.
  for (const run of runs) {
    if (!run.dirtyPhase) continue;
    for (const entry of run.dirtyPhase.queries) {
      const query = byId.get(entry.id);
      if (!query.requireSnapshotHash) continue;
      for (const gold of query.gold) {
        const headContent = readFileSync(
          join(FIXTURES, run.corpus, gold.path),
          "utf8",
        );
        const headHash = createHash("sha256").update(headContent).digest("hex");
        const hit = entry.results.find((h) => h.path === gold.path);
        if (hit && hit.revision_hash !== headHash) {
          policy.staleLeaks.push(entry.id);
        }
      }
    }
  }
  const warmP95Ms =
    warmAll.length > 0 ? Math.round(percentile(warmAll, 95)) : null;
  recall.recallAt5 = recall.eligible === 0 ? 0 : recall.found / recall.eligible;
  return { exact, recall, policy, provenance, roleFailed, warmP95Ms };
}

// ---------------------------------------------------------------------------
// Report.

function renderReport(results) {
  const lines = [];
  const push = (text = "") => lines.push(text);
  push("# Repoise offline lexical evaluation report");
  push();
  push(
    `Generated ${results.generatedAt} by 'scripts/eval/harness.mjs' on harness commit ${results.harness.harnessRepoSha}.`,
  );
  push();
  push("## Environment");
  push();
  const hw = results.hardware;
  push(
    `- OS: ${hw.os} ${hw.release}; CPU: ${hw.cpu} x${hw.cpuCount}; RAM: ${Math.round(hw.totalMemBytes / 1e9)} GiB; host ${hw.host}`,
  );
  push(
    `- Node ${hw.node}; CLI binary ${results.harness.binary} (version ${results.harness.cliVersion ?? "unknown"}); warm search iterations per query: ${results.harness.iterations}`,
  );
  push();
  push(
    "Timings are per-CLI-invocation wall time (process start to exit), including startup.",
  );
  push();
  push("## Runs (cold build, corpus/index sizes, warm search)");
  push();
  push(
    "| corpus | preset | cold build ms | source bytes | files | chunks | db bytes | warm p50 ms | warm p95 ms | check --fresh |",
  );
  push("| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |");
  for (const run of results.runs) {
    push(
      `| ${run.corpus} | ${run.preset} | ${run.coldBuildMs} | ${run.sourceBytes} | ${run.index?.files ?? "?"} | ${run.index?.chunks ?? "?"} | ${run.index?.dbBytes ?? "?"} | ${run.warmSearchMs?.p50 ?? "n/a"} | ${run.warmSearchMs?.p95 ?? "n/a"} | ${run.checkFreshExitCode} |`,
    );
  }
  push();
  const acmeLexical = results.runs.find(
    (r) => r.corpus === "acme" && r.preset === "docs-code-lexical",
  );
  const acmeDocs = results.runs.find(
    (r) => r.corpus === "acme" && r.preset === "docs-only",
  );
  if (acmeDocs?.presetProbes && acmeLexical) {
    push(
      "## Preset comparison (docs-only vs docs-code-lexical on the acme corpus)",
    );
    push();
    push(
      `docs-only: cold build ${acmeDocs.coldBuildMs} ms, ${acmeDocs.index?.files} files, ${acmeDocs.index?.chunks} chunks, ${acmeDocs.index?.dbBytes} db bytes. ` +
        `docs-code-lexical: cold build ${acmeLexical.coldBuildMs} ms, ${acmeLexical.index?.files} files, ${acmeLexical.index?.chunks} chunks, ${acmeLexical.index?.dbBytes} db bytes.`,
    );
    push();
    push("Code probes under docs-only (code hits must stay zero):");
    push();
    for (const probe of acmeDocs.presetProbes) {
      push(
        `- ${probe.probe}: ${probe.totalHits} hits, ${probe.codeHits} code hits`,
      );
    }
    push();
  }
  if (acmeLexical?.dirtyPhase) {
    const dirty = acmeLexical.dirtyPhase;
    push("## Dirty/stale phase (acme, after the uncommitted worktree edit)");
    push();
    push(
      `status: ${dirty.dirtyCount} dirty files, freshness ${dirty.freshness}; ` +
        `check --fresh exit code ${dirty.checkFreshExitCode} (3 = stale/missing).`,
    );
    push();
  }
  if (results.isolation) {
    push("## Scope isolation and stale exact reads (shared cache)");
    push();
    push(
      `Two corpora (acme, docs-site) indexed into one shared cache root before testing isolation. ${
        results.isolation.passed
          ? "All checks passed (foreign source ids rejected in both directions, in-scope read returned the exact snapshot text, stale read rejected after the file change)."
          : "CHECKS FAILED:"
      }`,
    );
    if (!results.isolation.passed) {
      for (const violation of results.isolation.violations)
        push(`- ${violation}`);
    }
    push();
    push("Unmeasured isolation cases (reported, not asserted):");
    for (const item of results.isolation.unmeasured) push(`- ${item}`);
    push();
  }
  push("## Query results");
  push();
  push(
    `Labeled queries: ${results.metrics.queryTotal} (${results.metrics.inTuning} in-tuning, ${results.metrics.heldOut} held-out).`,
  );
  push();
  const failed = results.runs.flatMap((r) =>
    [...r.queries, ...(r.dirtyPhase?.queries ?? [])]
      .filter((q) => q.status !== "pass")
      .map((q) => `- ${q.id}: ${q.reasons.join("; ")}`),
  );
  if (failed.length > 0) {
    push("Failed queries:");
    push();
    push(...failed);
    push();
  } else {
    push("All labeled queries passed their declared checks.");
    push();
  }
  const m = results.metrics;
  push("## Adoption gates (master plan section 11)");
  push();
  push("| gate | target | status | measured |");
  push("| --- | --- | --- | --- |");
  for (const gate of results.gates) {
    push(`| ${gate.id} | ${gate.target} | ${gate.status} | ${gate.measured} |`);
  }
  push();
  push(
    "Unmeasured gates stay unmeasured on purpose: hybrid presets need an operator-configured embedding provider, and the token/task gates need real agent replay evidence. This harness never infers or fabricates them.",
  );
  push();
  push("## Replay evidence (real agent pilot)");
  push();
  if (results.replayEvidence === null) {
    push(
      "None provided. Import a validated file with `--replay-evidence <file>` (format: `scripts/eval/replay-evidence.schema.json`).",
    );
  } else if (!results.replayEvidence.valid) {
    push(`Rejected import: ${results.replayEvidence.errors.join("; ")}`);
  } else {
    const s = results.replayEvidence.summary;
    push(
      `Accepted run ${s.runId} by ${s.operator} (model ${s.model.name} ${s.model.revision}, revision ${s.revision}).`,
    );
    push();
    push(
      "| condition | task entries | samples | model input tokens | task-level correct | tool calls | elapsed ms |",
    );
    push("| --- | --- | --- | --- | --- | --- | --- |");
    for (const [condition, entry] of Object.entries(s.conditions)) {
      push(
        `| ${condition} | ${entry.runs} | ${entry.sampleSize} | ${entry.modelInputTokens} | ${entry.correct}/${entry.runs} | ${entry.toolCalls} | ${Math.round(entry.elapsedMs)} |`,
      );
    }
    push();
    push(
      "`correct` is a task-level rate over task x condition entries (each `taskCorrect` is a task-level boolean); successful-repeat counts are not recorded and none are inferred. Token fields are totals over the declared `sampleSize` repeats.",
    );
    if (s.inputTokenReduction) {
      const r = s.inputTokenReduction;
      const reduction =
        r.reductionFraction === null
          ? "n/a"
          : `${(r.reductionFraction * 100).toFixed(1)}%`;
      push(
        `Derived at report time from measured values: mean per-sample model input tokens ${r.baselineMeanInputTokens.toFixed(1)} (${r.baseline}, ${r.baselineSamples} samples) vs ${r.candidateMeanInputTokens.toFixed(1)} (${r.candidate}, ${r.candidateSamples} samples); descriptive reduction ${reduction}. ${r.note}`,
      );
    }
  }
  push();
  return lines.join("\n") + "\n";
}

// ---------------------------------------------------------------------------
// Main.

function parseArgs(argv) {
  const options = {
    out: null,
    iterations: 21,
    binary: null,
    replayEvidence: null,
  };
  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    if (arg === "--out") options.out = argv[++i];
    else if (arg === "--iterations") options.iterations = Number(argv[++i]);
    else if (arg === "--binary") options.binary = argv[++i];
    else if (arg === "--replay-evidence") options.replayEvidence = argv[++i];
    else if (arg === "--help") options.help = true;
    else throw new Error(`unknown argument: ${arg}`);
  }
  return options;
}

async function main() {
  const options = parseArgs(process.argv.slice(2));
  if (options.help) {
    console.log(
      "usage: node scripts/eval/harness.mjs [--out DIR] [--iterations N] [--binary PATH] [--replay-evidence FILE]",
    );
    return;
  }
  const manifest = loadJson(join(FIXTURES, "manifest.json"));
  const queries = loadJson(join(HERE, "queries.json")).queries;
  // Fail closed on replay evidence before any indexing work: a malformed or
  // invalid import stops the run with clear diagnostics and a nonzero exit.
  let replayEvidence = null;
  if (options.replayEvidence !== null) {
    let doc;
    try {
      doc = loadJson(options.replayEvidence);
    } catch (err) {
      console.error(
        `replay evidence: cannot parse ${options.replayEvidence}: ${err.message}`,
      );
      process.exit(2);
    }
    const validation = validateReplayEvidence(doc);
    if (!validation.ok) {
      for (const message of validation.errors) {
        console.error(`replay evidence: ${message}`);
      }
      console.error(
        `replay evidence: rejecting ${options.replayEvidence} (${validation.errors.length} validation error(s))`,
      );
      process.exit(2);
    }
    replayEvidence = { valid: true, summary: summarizeReplayEvidence(doc) };
  }
  const binary =
    options.binary ??
    process.env.REPOISE_BIN ??
    join(REPO_ROOT, "target", "debug", "repoise");
  if (!existsSync(binary)) {
    throw new Error(
      `CLI binary not found at ${binary}; build with cargo build --package repoise-cli or pass --binary`,
    );
  }
  const version = runCli(binary, ["version"], REPO_ROOT, process.env);
  const cliVersion = version.stdout.trim().replace(/^repoise\s+/u, "");
  const harnessRepoSha = spawnSync("git", ["rev-parse", "HEAD"], {
    cwd: REPO_ROOT,
    encoding: "utf8",
  }).stdout.trim();

  const runs = [];
  for (const spec of PRESET_RUNS) {
    const cache = mkdtempSync(
      join(tmpdir(), `repoise-eval-cache-${spec.corpus}-${spec.preset}-`),
    );
    const env = { ...process.env, REPOISE_CACHE_DIR: cache };
    console.log(`running ${spec.corpus} / ${spec.preset} ...`);
    const run = runOne(
      spec,
      manifest,
      queries,
      binary,
      env,
      options.iterations,
    );
    runs.push(run);
    rmSync(cache, { recursive: true, force: true });
  }

  const metrics = assembleMetrics(runs, queries);
  console.log("running shared-cache isolation phase ...");
  metrics.isolation = runIsolationPhase(binary, manifest);
  const gates = evaluateGates(
    {
      exact: metrics.exact,
      recall: metrics.recall,
      policy: metrics.policy,
      provenance: metrics.provenance,
      warmP95Ms: metrics.warmP95Ms,
      isolation: metrics.isolation,
    },
    replayEvidence?.summary ?? null,
  );
  const inTuning = queries.filter((q) => !q.heldOut).length;
  const results = {
    schemaVersion: 1,
    generatedAt: new Date().toISOString(),
    harness: {
      binary,
      cliVersion,
      harnessRepoSha,
      iterations: options.iterations,
      manifestRevision: manifest.revision,
    },
    hardware: {
      os: platform(),
      release: release(),
      cpu: cpus()[0]?.model ?? "unknown",
      cpuCount: cpus().length,
      totalMemBytes: totalmem(),
      freeMemBytes: freemem(),
      host: hostname(),
      node: process.version,
    },
    declaredSettings: {
      presetRuns: PRESET_RUNS,
      notes:
        "Preset is declared per run via the CLI --preset flag; acme additionally commits repoise.config.json enabling the local history lane (horizon 50). All other settings are engine defaults (max file 1 MiB, .repoise cache dir, rrf k 60).",
    },
    runs,
    metrics: {
      ...metrics,
      queryTotal: queries.length,
      inTuning,
      heldOut: queries.length - inTuning,
    },
    gates,
    isolation: metrics.isolation,
    replayEvidence,
  };
  const outDir = options.out ?? join(REPO_ROOT, "eval-results");
  mkdirSync(outDir, { recursive: true });
  writeFileSync(
    join(outDir, "results.json"),
    `${JSON.stringify(results, null, 2)}\n`,
  );
  writeFileSync(join(outDir, "report.md"), renderReport(results));
  const failedGates = gates.filter((g) => g.status === "measured-failed");
  console.log(
    `wrote ${join(outDir, "results.json")} and ${join(outDir, "report.md")}`,
  );
  console.log(
    `gates: ${gates.filter((g) => g.status === "measured-passed").length} passed, ${failedGates.length} failed, ${gates.filter((g) => g.status === "unmeasured").length} unmeasured`,
  );
  if (failedGates.length > 0) {
    for (const gate of failedGates)
      console.log(`FAILED ${gate.id}: ${gate.measured}`);
    process.exitCode = 1;
  }
}

// Runs only when executed directly (the test suite imports the pure helpers).
if (process.argv[1] === fileURLToPath(import.meta.url)) {
  main().catch((err) => {
    console.error(`error: ${err.message}`);
    process.exit(1);
  });
}
