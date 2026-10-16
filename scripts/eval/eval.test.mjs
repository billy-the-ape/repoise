// Tests for the offline lexical evaluation harness: frozen fixture integrity,
// labeled-query invariants, metric helpers, gate evaluation, the shared-cache
// isolation judgement and the replay-evidence import format. Run by `npm test`
// (`node --test scripts/release/*.test.mjs scripts/eval/*.test.mjs`).

import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import {
  existsSync,
  readdirSync,
  readFileSync,
  rmSync,
  statSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import test from "node:test";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import {
  evaluateGates,
  evidenceInTop5,
  historyEvidenceInTop5,
  judgeIsolation,
  percentile,
  secretMarkerLeakEntryIds,
} from "./harness.mjs";
import { summarizeReplayEvidence, validateReplayEvidence } from "./replay.mjs";

const HERE = dirname(fileURLToPath(import.meta.url));
const FIXTURES = join(HERE, "fixtures");

const loadJson = (path) => JSON.parse(readFileSync(path, "utf8"));
const sha256File = (path) =>
  createHash("sha256").update(readFileSync(path)).digest("hex");

function walkFiles(dir, prefix = "") {
  const out = [];
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const rel = prefix ? `${prefix}/${entry.name}` : entry.name;
    if (entry.isDirectory()) out.push(...walkFiles(join(dir, entry.name), rel));
    else out.push(rel);
  }
  return out;
}

const manifest = loadJson(join(FIXTURES, "manifest.json"));
const queryDoc = loadJson(join(HERE, "queries.json"));

test("fixture manifest is frozen and complete", () => {
  assert.equal(manifest.manifestVersion, 1);
  for (const [key, frozen] of Object.entries(manifest.files)) {
    const path = join(FIXTURES, key);
    assert.ok(existsSync(path), `missing fixture file ${key}`);
    assert.equal(
      sha256File(path),
      frozen,
      `fixture ${key} does not match the manifest freeze`,
    );
  }
  // No unlisted fixture content (docs and the manifest itself are allowed).
  const listed = new Set(Object.keys(manifest.files));
  for (const rel of walkFiles(FIXTURES)) {
    assert.ok(
      listed.has(rel) || rel === "manifest.json" || rel.endsWith("README.md"),
      `unlisted fixture file ${rel} (add it to the manifest or remove it)`,
    );
  }
  for (const [name, corpus] of Object.entries(manifest.corpora)) {
    const refs = new Set();
    for (const commit of corpus.commits) {
      assert.ok(
        !refs.has(commit.ref),
        `duplicate revision ref ${commit.ref} in ${name}`,
      );
      refs.add(commit.ref);
      for (const rel of commit.files) {
        const known =
          `${name}/${rel}` in manifest.files ||
          commit.fileContents[rel] !== undefined;
        assert.ok(known, `commit ${commit.ref} references unknown file ${rel}`);
      }
    }
    for (const op of corpus.dirty ?? []) {
      assert.ok(
        `${name}/${op.path}` in manifest.files,
        `dirty op for ${name} references unknown file ${op.path}`,
      );
    }
  }
});

test("labeled queries satisfy the declared invariants", () => {
  assert.equal(queryDoc.schemaVersion, 1);
  const queries = queryDoc.queries;
  const ids = new Set();
  const categories = new Set();
  let inTuning = 0;
  let heldOut = 0;
  for (const query of queries) {
    assert.ok(!ids.has(query.id), `duplicate query id ${query.id}`);
    ids.add(query.id);
    assert.ok(
      ["acme", "docs-site", "pyrepo"].includes(query.corpus),
      `unknown corpus ${query.corpus}`,
    );
    assert.ok(
      ["docs-only", "docs-code-lexical"].includes(query.preset),
      `unknown preset ${query.preset}`,
    );
    assert.ok(
      ["search", "history"].includes(query.lane),
      `unknown lane ${query.lane}`,
    );
    assert.ok(
      ["exact", "conceptual", "policy", "diagnostic"].includes(query.kind),
      `unknown kind ${query.kind}`,
    );
    assert.equal(typeof query.heldOut, "boolean");
    assert.ok(query.query.length > 0, `empty query text for ${query.id}`);
    categories.add(query.category);
    if (query.heldOut) heldOut += 1;
    else inTuning += 1;
    if (query.kind === "exact" || query.kind === "conceptual") {
      assert.ok(
        Array.isArray(query.gold) && query.gold.length > 0,
        `missing gold for ${query.id}`,
      );
      for (const gold of query.gold) {
        assert.ok(
          `${query.corpus}/${gold.path}` in manifest.files,
          `gold path ${gold.path} not in ${query.corpus}`,
        );
        if (query.lane === "search") {
          const lines = readFileSync(
            join(FIXTURES, query.corpus, gold.path),
            "utf8",
          )
            .replace(/\n$/, "")
            .split("\n").length;
          assert.ok(
            gold.line_start >= 1 &&
              gold.line_end >= gold.line_start &&
              gold.line_end <= lines,
            `gold range ${gold.line_start}-${gold.line_end} out of bounds for ${gold.path} (${lines} lines)`,
          );
        } else {
          assert.ok(
            manifest.corpora[query.corpus].commits.some(
              (c) => c.ref === gold.revisionRef,
            ),
            `unknown revision ref ${gold.revisionRef} for ${query.id}`,
          );
        }
      }
    }
  }
  assert.ok(
    inTuning >= 40,
    `need at least 40 in-tuning queries, got ${inTuning}`,
  );
  assert.ok(heldOut >= 10, `need at least 10 held-out queries, got ${heldOut}`);
  for (const category of [
    "exact-identifier",
    "unknown-concept",
    "current-vs-plan",
    "history",
    "code-behavior",
    "tests",
    "unsupported-language",
    "docs-only",
    "dirty-stale",
    "secret-policy",
    "cross-scope",
  ]) {
    assert.ok(categories.has(category), `missing planned category ${category}`);
  }
  assert.ok(
    queries.some((q) => q.heldOut && q.kind === "conceptual"),
    "held-out conceptual queries are required for the recall gate",
  );
});

test("percentile uses nearest-rank semantics", () => {
  assert.equal(percentile([10, 20, 30, 40, 50], 50), 30);
  assert.equal(percentile([10, 20, 30, 40, 50], 95), 50);
  assert.equal(percentile([7], 50), 7);
  assert.equal(percentile([], 50), null);
});

test("evidence matching requires path and line-range overlap", () => {
  const hits = [
    { path: "a.md", line_start: 1, line_end: 5 },
    { path: "b.md", line_start: 10, line_end: 20 },
  ];
  assert.ok(
    evidenceInTop5(hits, [{ path: "a.md", line_start: 4, line_end: 6 }]),
  );
  assert.ok(
    evidenceInTop5(hits, [
      { path: "c.md", line_start: 1, line_end: 2 },
      { path: "b.md", line_start: 20, line_end: 30 },
    ]),
  );
  assert.equal(
    evidenceInTop5(hits, [{ path: "a.md", line_start: 6, line_end: 9 }]),
    false,
  );
  const revisions = { c1: "aaa", c2: "bbb" };
  const historyHits = [{ revision_id: "git:bbb", affected_paths: ["x.py"] }];
  assert.ok(
    historyEvidenceInTop5(
      historyHits,
      [{ revisionRef: "c2", path: "x.py" }],
      revisions,
    ),
  );
  assert.equal(
    historyEvidenceInTop5(
      historyHits,
      [{ revisionRef: "c1", path: "x.py" }],
      revisions,
    ),
    false,
  );
});

test("gates are measured or explicitly unmeasured, never invented", () => {
  const passing = {
    exact: { total: 2, passed: 2, failed: [] },
    recall: { eligible: 10, found: 9, recallAt5: 0.9 },
    policy: {
      secretLeaks: [],
      crossScopeLeaks: [],
      staleLeaks: [],
      mustNotViolations: [],
    },
    provenance: { validatedHits: 5, violations: [] },
    isolation: { passed: true, violations: [] },
    warmP95Ms: 120,
  };
  const gates = evaluateGates(passing, null);
  const byId = Object.fromEntries(gates.map((g) => [g.id, g.status]));
  assert.equal(byId["G1-exact-lookup"], "measured-passed");
  assert.equal(byId["G2-policy"], "measured-passed");
  assert.equal(byId["G3-recall"], "measured-passed");
  assert.equal(byId["G4-warm-search"], "measured-passed");
  assert.equal(byId["G5-token-reduction"], "unmeasured");
  assert.equal(byId["G6-code-vectors"], "unmeasured");
  assert.equal(byId["G7-hybrid-latency"], "unmeasured");
  const failing = evaluateGates(
    { ...passing, recall: { eligible: 10, found: 8, recallAt5: 0.8 } },
    null,
  );
  assert.equal(
    failing.find((g) => g.id === "G3-recall").status,
    "measured-failed",
  );
});

test("the policy gate is unmeasured when the isolation phase did not run", () => {
  const { isolation, ...passing } = {
    exact: { total: 2, passed: 2, failed: [] },
    recall: { eligible: 10, found: 9, recallAt5: 0.9 },
    policy: {
      secretLeaks: [],
      crossScopeLeaks: [],
      staleLeaks: [],
      mustNotViolations: [],
    },
    provenance: { validatedHits: 5, violations: [] },
    isolation: { passed: true, violations: [] },
    warmP95Ms: 120,
  };
  const gates = evaluateGates(passing, null);
  const gate = gates.find((g) => g.id === "G2-policy");
  assert.equal(gate.status, "unmeasured");
  assert.ok(gate.measured.includes("shared-cache isolation phase was not run"));
  // With isolation present but a violation injected, the gate fails.
  const violated = evaluateGates(
    {
      ...passing,
      isolation: {
        passed: false,
        violations: ["foreign source id was accepted"],
      },
    },
    null,
  );
  assert.equal(
    violated.find((g) => g.id === "G2-policy").status,
    "measured-failed",
  );
});

test("the secret sentinel scan detects leaked hit text for any query", () => {
  const clean = [
    {
      id: "q1",
      results: [{ path: "a.md", title: "Guide", excerpt: "setup text" }],
    },
    { id: "q2", results: [] },
  ];
  assert.deepEqual(secretMarkerLeakEntryIds(clean), []);
  const leaked = [
    {
      id: "q1",
      results: [{ path: "a.md", title: "Guide", excerpt: "setup text" }],
    },
    {
      id: "q3",
      results: [
        {
          path: "b.md",
          title: "Notes",
          excerpt: "contains REPOISE-FAKE-SECRET-0001 inside",
        },
      ],
    },
  ];
  assert.deepEqual(secretMarkerLeakEntryIds(leaked), ["q3"]);
});

function sampleReplay() {
  return {
    schemaVersion: 1,
    runId: "replay-2026-10-12-001",
    recordedAt: "2026-10-12T10:00:00Z",
    operator: "pilot-operator",
    fixed: {
      model: { name: "example-model", revision: "2026-10-01" },
      tasks: [
        { id: "task-1", correctnessCriteria: "hidden test suite passes" },
        { id: "task-2", correctnessCriteria: "deterministic review check" },
      ],
      revision: "git:0123456789abcdef0123456789abcdef01234567",
      budgets: { contextCapTokens: 200000, maxOutputTokens: 8192 },
      promptVersion: "v3",
    },
    conditions: ["curated-docs-rg", "lexical"],
    runs: [
      {
        task: "task-1",
        condition: "curated-docs-rg",
        sampleSize: 3,
        modelInputTokens: 9000,
        modelOutputTokens: 800,
        retrievalOutputTokens: 4000,
        toolCalls: 12,
        elapsedMs: 45000,
        taskCorrect: true,
        retrievalErrors: 0,
        staleEvidence: 0,
      },
      {
        task: "task-1",
        condition: "lexical",
        sampleSize: 3,
        modelInputTokens: 6000,
        modelOutputTokens: 700,
        retrievalOutputTokens: 2500,
        toolCalls: 9,
        elapsedMs: 41000,
        taskCorrect: true,
        retrievalErrors: 0,
        staleEvidence: 0,
      },
      {
        task: "task-2",
        condition: "curated-docs-rg",
        sampleSize: 2,
        modelInputTokens: 12000,
        modelOutputTokens: 900,
        retrievalOutputTokens: 5000,
        toolCalls: 14,
        elapsedMs: 60000,
        taskCorrect: false,
        retrievalErrors: 1,
        staleEvidence: 0,
      },
      {
        task: "task-2",
        condition: "lexical",
        sampleSize: 2,
        modelInputTokens: 8000,
        modelOutputTokens: 800,
        retrievalOutputTokens: 3000,
        toolCalls: 10,
        elapsedMs: 52000,
        taskCorrect: true,
        retrievalErrors: 0,
        staleEvidence: 0,
      },
    ],
  };
}

test("valid replay evidence passes and summarizes measured values only", () => {
  const doc = sampleReplay();
  const validation = validateReplayEvidence(doc);
  assert.deepEqual(validation, { ok: true, errors: [] });
  const summary = summarizeReplayEvidence(doc);
  assert.equal(summary.conditions.lexical.runs, 2);
  assert.equal(summary.conditions.lexical.sampleSize, 5);
  assert.equal(summary.conditions.lexical.modelInputTokens, 14000);
  assert.ok(summary.inputTokenReduction);
  assert.ok(summary.inputTokenReduction.reductionFraction > 0.2);
});

test("fabricated or inferred fields are rejected", () => {
  const doc = sampleReplay();
  doc.runs[0].tokenSavingsPercent = 33; // derived value: not importable
  const validation = validateReplayEvidence(doc);
  assert.equal(validation.ok, false);
  assert.ok(validation.errors.some((e) => e.includes("tokenSavingsPercent")));
});

test("missing task x condition matrix entries are rejected", () => {
  const doc = sampleReplay();
  doc.runs = doc.runs.filter(
    (run) => !(run.task === "task-2" && run.condition === "lexical"),
  );
  const validation = validateReplayEvidence(doc);
  assert.equal(validation.ok, false);
  assert.ok(validation.errors.some((e) => e.includes("missing measured run")));
});

test("non-measured types are rejected", () => {
  const doc = sampleReplay();
  doc.runs[0].modelInputTokens = 9000.5;
  doc.runs[1].taskCorrect = "probably";
  doc.recordedAt = "2026-10-12";
  const validation = validateReplayEvidence(doc);
  assert.equal(validation.ok, false);
  assert.ok(validation.errors.length >= 3);
});

test("unknown conditions and tasks are rejected", () => {
  const doc = sampleReplay();
  doc.conditions = ["curated-docs-rg", "rag-fantasy"];
  doc.runs[1].condition = "rag-fantasy";
  doc.runs[2].task = "task-99";
  const validation = validateReplayEvidence(doc);
  assert.equal(validation.ok, false);
  assert.ok(validation.errors.some((e) => e.includes("rag-fantasy")));
  assert.ok(validation.errors.some((e) => e.includes("task-99")));
});

test("replay schema file and validator agree on the field set", () => {
  const schema = loadJson(join(HERE, "replay-evidence.schema.json"));
  assert.equal(
    schema.title,
    "Repoise real-agent replay evidence (import format v1)",
  );
  const runFields = Object.keys(schema.properties.runs.items.properties);
  for (const field of [
    "modelInputTokens",
    "modelOutputTokens",
    "retrievalOutputTokens",
    "toolCalls",
    "elapsedMs",
    "taskCorrect",
  ]) {
    assert.ok(runFields.includes(field), `schema missing run field ${field}`);
  }
  // The schema must not allow any derived/savings-style field.
  assert.ok(
    !runFields.some((f) => /savings|saving|estimated|inferred/i.test(f)),
  );
});

test("queries.json and manifest.json parse as strict JSON", () => {
  assert.ok(Array.isArray(queryDoc.queries) && queryDoc.queries.length >= 50);
  assert.equal(Object.keys(manifest.corpora).length, 3);
});

function oneRun(task, condition, sampleSize, modelInputTokens, taskCorrect) {
  return {
    task,
    condition,
    sampleSize,
    modelInputTokens,
    modelOutputTokens: 10,
    retrievalOutputTokens: 10,
    toolCalls: 3,
    elapsedMs: 1000,
    taskCorrect,
    retrievalErrors: 0,
    staleEvidence: 0,
  };
}

function twoTaskDoc(runs) {
  const doc = sampleReplay();
  doc.fixed.tasks = [
    { id: "task-1", correctnessCriteria: "check" },
    { id: "task-2", correctnessCriteria: "check" },
  ];
  doc.runs = runs;
  return doc;
}

test("reviewer example: unequal sample sizes give +33.33%, not -100%", () => {
  // Baseline 3000 tokens over 1 sample vs candidate 6000 tokens over 3
  // samples (2000 per sample): true per-sample reduction is 33.33%.
  const doc = twoTaskDoc([
    oneRun("task-1", "curated-docs-rg", 1, 3000, true),
    oneRun("task-1", "lexical", 3, 6000, true),
    oneRun("task-2", "curated-docs-rg", 1, 3000, true),
    oneRun("task-2", "lexical", 3, 6000, true),
  ]);
  const summary = summarizeReplayEvidence(doc);
  const reduction = summary.inputTokenReduction;
  assert.equal(reduction.candidate, "lexical");
  assert.equal(reduction.baseline, "curated-docs-rg");
  assert.equal(reduction.baselineSamples, 2);
  assert.equal(reduction.candidateSamples, 6);
  assert.equal(reduction.baselineMeanInputTokens, 3000);
  assert.equal(reduction.candidateMeanInputTokens, 2000);
  assert.ok(Math.abs(reduction.reductionFraction - 1 / 3) < 1e-9);
});

test("per-sample means use the summed sampleSize when sizes differ per task", () => {
  // sampleReplay: task-1 x3, task-2 x2. Baseline total 21000 over 5 samples
  // (mean 4200), candidate total 14000 over 5 samples (mean 2800).
  const summary = summarizeReplayEvidence(sampleReplay());
  const reduction = summary.inputTokenReduction;
  assert.equal(reduction.baselineSamples, 5);
  assert.equal(reduction.candidateSamples, 5);
  assert.equal(reduction.baselineMeanInputTokens, 4200);
  assert.equal(reduction.candidateMeanInputTokens, 2800);
  assert.ok(Math.abs(reduction.reductionFraction - 1 / 3) < 1e-9);
});

test("zero-token baseline yields a null reduction, not a division by zero", () => {
  const doc = twoTaskDoc([
    oneRun("task-1", "curated-docs-rg", 2, 0, true),
    oneRun("task-1", "lexical", 2, 4000, true),
    oneRun("task-2", "curated-docs-rg", 1, 0, false),
    oneRun("task-2", "lexical", 1, 2000, true),
  ]);
  const reduction = summarizeReplayEvidence(doc).inputTokenReduction;
  assert.equal(reduction.baselineMeanInputTokens, 0);
  assert.equal(reduction.candidateMeanInputTokens, 2000);
  assert.equal(reduction.reductionFraction, null);
});

test("taskCorrect aggregates as a task-level rate, never per-sample", () => {
  const summary = summarizeReplayEvidence(sampleReplay());
  // sampleReplay: task-2 is incorrect only under curated-docs-rg.
  assert.equal(summary.conditions["curated-docs-rg"].taskCorrectRate, 0.5);
  assert.equal(summary.conditions.lexical.taskCorrectRate, 1);
  assert.equal(summary.conditions["curated-docs-rg"].correct, 1);
  // The rate must not move when sample sizes change (no repeat counts inferred).
  const doubled = sampleReplay();
  for (const run of doubled.runs) run.sampleSize *= 4;
  const summary2 = summarizeReplayEvidence(doubled);
  assert.equal(summary2.conditions["curated-docs-rg"].taskCorrectRate, 0.5);
  assert.equal(summary2.conditions["curated-docs-rg"].sampleSize, 20);
});

test("null task entries are rejected with validation errors, not a throw", () => {
  const doc = sampleReplay();
  doc.fixed.tasks = [null];
  let validation;
  assert.doesNotThrow(() => {
    validation = validateReplayEvidence(doc);
  });
  assert.equal(validation.ok, false);
  assert.ok(
    validation.errors.some((e) =>
      e.includes("fixed.tasks[0] must be an object"),
    ),
  );
});

test("duplicate task ids are rejected", () => {
  const doc = sampleReplay();
  doc.fixed.tasks = [
    { id: "task-1", correctnessCriteria: "check" },
    { id: "task-1", correctnessCriteria: "check" },
  ];
  const validation = validateReplayEvidence(doc);
  assert.equal(validation.ok, false);
  assert.ok(validation.errors.some((e) => e.includes("duplicate task id")));
});

test("harness exits nonzero with a diagnostic on invalid --replay-evidence", () => {
  const bad = join(tmpdir(), `repoise-eval-bad-replay-${process.pid}.json`);
  writeFileSync(
    bad,
    JSON.stringify({
      ...sampleReplay(),
      fixed: { ...sampleReplay().fixed, tasks: [null] },
    }),
  );
  try {
    const result = spawnSync(
      process.execPath,
      [join(HERE, "harness.mjs"), "--replay-evidence", bad],
      {
        encoding: "utf8",
        timeout: 60_000,
      },
    );
    assert.notEqual(
      result.status,
      0,
      `expected nonzero exit, stderr: ${result.stderr}`,
    );
    assert.ok(
      result.stderr.includes("replay evidence"),
      `diagnostic missing: ${result.stderr}`,
    );
  } finally {
    rmSync(bad, { force: true });
  }
});

test("harness exits nonzero with a diagnostic on malformed (non-JSON) replay evidence", () => {
  const bad = join(
    tmpdir(),
    `repoise-eval-malformed-replay-${process.pid}.json`,
  );
  writeFileSync(bad, "{ this is not json");
  try {
    const result = spawnSync(
      process.execPath,
      [join(HERE, "harness.mjs"), "--replay-evidence", bad],
      {
        encoding: "utf8",
        timeout: 60_000,
      },
    );
    assert.notEqual(
      result.status,
      0,
      `expected nonzero exit, stderr: ${result.stderr}`,
    );
    assert.ok(
      result.stderr.includes("replay evidence"),
      `diagnostic missing: ${result.stderr}`,
    );
  } finally {
    rmSync(bad, { force: true });
  }
});

function isolationChecks(overrides = {}) {
  return {
    foreignReads: [
      {
        label: "docs-site source id read from the acme scope",
        result: { code: 1, stdout: "" },
        foreignSentinel: "steward before publication",
      },
      {
        label: "acme source id read from the docs-site scope",
        result: { code: 1, stdout: "" },
        foreignSentinel: "computeBatchId",
      },
    ],
    goodRead: { code: 0, stdout: "", text: "expected snapshot text" },
    goodReadExpectedText: "expected snapshot text",
    staleRead: {
      code: 1,
      stdout: "",
      stderr:
        "stale source (docs/current/architecture.md): content hash changed since the index",
    },
    dirtySentinel: "dirtysentinelheron uncommitted isolation change",
    ...overrides,
  };
}

test("isolation judgement passes on clean rejections and exact snapshot content", () => {
  assert.deepEqual(judgeIsolation(isolationChecks()), {
    passed: true,
    violations: [],
  });
});

test("isolation judgement detects injected foreign-read and stale-read leaks", () => {
  const accepted = isolationChecks();
  accepted.foreignReads[0].result.code = 0;
  const acceptedVerdict = judgeIsolation(accepted);
  assert.equal(acceptedVerdict.passed, false);
  assert.ok(acceptedVerdict.violations.some((v) => v.includes("accepted")));

  const leaking = isolationChecks();
  leaking.foreignReads[1].result.stdout = "computeBatchId"; // leaked other-scope content
  const leakingVerdict = judgeIsolation(leaking);
  assert.equal(leakingVerdict.passed, false);
  assert.ok(
    leakingVerdict.violations.some((v) =>
      v.includes("leaked the other scope's content"),
    ),
  );

  const staleAccepted = isolationChecks();
  staleAccepted.staleRead.code = 0;
  const staleVerdict = judgeIsolation(staleAccepted);
  assert.equal(staleVerdict.passed, false);
  assert.ok(
    staleVerdict.violations.some((v) => v.includes("stale read was accepted")),
  );

  const staleLeaking = isolationChecks();
  staleLeaking.staleRead.stderr =
    staleLeaking.staleRead.stderr + " " + staleLeaking.dirtySentinel;
  const staleLeakVerdict = judgeIsolation(staleLeaking);
  assert.equal(staleLeakVerdict.passed, false);
  assert.ok(
    staleLeakVerdict.violations.some((v) =>
      v.includes("leaked the newly written"),
    ),
  );

  const wrongText = isolationChecks();
  wrongText.goodRead.text = "something else entirely";
  const wrongTextVerdict = judgeIsolation(wrongText);
  assert.equal(wrongTextVerdict.passed, false);
  assert.ok(
    wrongTextVerdict.violations.some((v) => v.includes("exact snapshot text")),
  );
});
