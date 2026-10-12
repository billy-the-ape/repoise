// Validates real-agent replay evidence against the import format
// (replay-evidence.schema.json, v1) and summarizes measured values only.
// No schema engine dependency; the validator mirrors the schema exactly,
// including rejection of unknown properties (so derived/inferred fields such
// as token-savings values cannot be imported).

const CONDITIONS = [
  "rg-only",
  "curated-docs-rg",
  "lexical",
  "docs-hybrid",
  "docs-code-lexical",
  "docs-code-hybrid",
];

const isNonEmptyString = (value) => typeof value === "string" && value.length > 0;
const isInteger = (value) => Number.isInteger(value);
const isNumber = (value) => typeof value === "number" && Number.isFinite(value);

function rejectUnknown(object, allowed, errors, where) {
  for (const key of Object.keys(object)) {
    if (!allowed.includes(key)) {
      errors.push(`${where}: unknown field "${key}" (derived/inferred values are not importable)`);
    }
  }
}

/// Validates one replay-evidence document. Returns `{ ok, errors }`.
export function validateReplayEvidence(doc) {
  const errors = [];
  if (typeof doc !== "object" || doc === null || Array.isArray(doc)) {
    return { ok: false, errors: ["document must be a JSON object"] };
  }
  rejectUnknown(doc, ["schemaVersion", "runId", "recordedAt", "operator", "fixed", "conditions", "runs"], errors, "root");
  if (doc.schemaVersion !== 1) errors.push("schemaVersion must be 1");
  if (!isNonEmptyString(doc.runId)) errors.push("runId must be a non-empty string");
  if (typeof doc.recordedAt !== "string" || !/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(\.\d+)?Z$/.test(doc.recordedAt)) {
    errors.push("recordedAt must be an RFC 3339 UTC timestamp ending in Z");
  }
  if (!isNonEmptyString(doc.operator)) errors.push("operator must be a non-empty string");

  const fixed = doc.fixed;
  if (typeof fixed !== "object" || fixed === null || Array.isArray(fixed)) {
    errors.push("fixed must be an object");
  } else {
    rejectUnknown(
      fixed,
      ["model", "tasks", "revision", "budgets", "promptVersion"],
      errors,
      "fixed",
    );
    const model = fixed.model;
    if (typeof model !== "object" || model === null || Array.isArray(model)) {
      errors.push("fixed.model must be an object");
    } else {
      rejectUnknown(model, ["name", "revision"], errors, "fixed.model");
      if (!isNonEmptyString(model.name)) errors.push("fixed.model.name must be a non-empty string");
      if (!isNonEmptyString(model.revision)) errors.push("fixed.model.revision must be a non-empty string");
    }
    if (!Array.isArray(fixed.tasks) || fixed.tasks.length < 1) {
      errors.push("fixed.tasks must be a non-empty array");
    } else {
      for (const [i, task] of fixed.tasks.entries()) {
        rejectUnknown(task, ["id", "correctnessCriteria"], errors, `fixed.tasks[${i}]`);
        if (!isNonEmptyString(task?.id)) errors.push(`fixed.tasks[${i}].id must be a non-empty string`);
        if (!isNonEmptyString(task?.correctnessCriteria)) {
          errors.push(`fixed.tasks[${i}].correctnessCriteria must be a non-empty string`);
        }
      }
    }
    if (!isNonEmptyString(fixed.revision)) errors.push("fixed.revision must be a non-empty string");
    const budgets = fixed.budgets;
    if (typeof budgets !== "object" || budgets === null || Array.isArray(budgets)) {
      errors.push("fixed.budgets must be an object");
    } else {
      rejectUnknown(budgets, ["contextCapTokens", "maxOutputTokens", "maxToolCalls"], errors, "fixed.budgets");
      for (const key of ["contextCapTokens", "maxOutputTokens"]) {
        if (!isInteger(budgets[key]) || budgets[key] < 1) {
          errors.push(`fixed.budgets.${key} must be an integer >= 1`);
        }
      }
      if (budgets.maxToolCalls !== undefined && (!isInteger(budgets.maxToolCalls) || budgets.maxToolCalls < 1)) {
        errors.push("fixed.budgets.maxToolCalls must be an integer >= 1");
      }
    }
    if (!isNonEmptyString(fixed.promptVersion)) errors.push("fixed.promptVersion must be a non-empty string");
  }

  if (!Array.isArray(doc.conditions) || doc.conditions.length < 2) {
    errors.push("conditions must declare at least two conditions");
  } else if (new Set(doc.conditions).size !== doc.conditions.length) {
    errors.push("conditions must be unique");
  } else {
    for (const condition of doc.conditions) {
      if (!CONDITIONS.includes(condition)) errors.push(`unknown condition "${condition}"`);
    }
  }

  const runs = doc.runs;
  if (!Array.isArray(runs) || runs.length < 1) {
    errors.push("runs must be a non-empty array");
  } else {
    const seen = new Set();
    const taskIds = new Set(
      Array.isArray(fixed?.tasks) ? (fixed.tasks.map((task) => task?.id).filter(Boolean)) : [],
    );
    const declared = Array.isArray(doc.conditions) ? doc.conditions : [];
    for (const [i, run] of runs.entries()) {
      const where = `runs[${i}]`;
      if (typeof run !== "object" || run === null || Array.isArray(run)) {
        errors.push(`${where} must be an object`);
        continue;
      }
      rejectUnknown(
        run,
        [
          "task",
          "condition",
          "sampleSize",
          "modelInputTokens",
          "modelOutputTokens",
          "retrievalOutputTokens",
          "toolCalls",
          "elapsedMs",
          "taskCorrect",
          "retrievalErrors",
          "staleEvidence",
        ],
        errors,
        where,
      );
      if (!isNonEmptyString(run.task)) errors.push(`${where}.task must be a non-empty string`);
      else if (taskIds.size > 0 && !taskIds.has(run.task)) {
        errors.push(`${where}.task "${run.task}" is not in fixed.tasks`);
      }
      if (!CONDITIONS.includes(run.condition)) errors.push(`${where}.condition must be a declared matrix condition`);
      else if (!declared.includes(run.condition)) {
        errors.push(`${where}.condition "${run.condition}" is not in the declared conditions`);
      }
      if (!isInteger(run.sampleSize) || run.sampleSize < 1) {
        errors.push(`${where}.sampleSize must be an integer >= 1`);
      }
      for (const key of [
        "modelInputTokens",
        "modelOutputTokens",
        "retrievalOutputTokens",
        "toolCalls",
        "retrievalErrors",
        "staleEvidence",
      ]) {
        if (!isInteger(run[key]) || run[key] < 0) {
          errors.push(`${where}.${key} must be a measured integer >= 0 (never inferred)`);
        }
      }
      if (!isNumber(run.elapsedMs) || run.elapsedMs < 0) {
        errors.push(`${where}.elapsedMs must be a measured number >= 0`);
      }
      if (typeof run.taskCorrect !== "boolean") {
        errors.push(`${where}.taskCorrect must be a measured boolean (never guessed)`);
      }
      const key = `${run.task}|${run.condition}`;
      if (seen.has(key)) errors.push(`duplicate run for ${key}`);
      seen.add(key);
    }
    if (errors.length === 0) {
      for (const task of taskIds) {
        for (const condition of declared) {
          if (!seen.has(`${task}|${condition}`)) {
            errors.push(`missing measured run for task ${task} under condition ${condition}`);
          }
        }
      }
    }
  }
  return { ok: errors.length === 0, errors };
}

/// Aggregates measured per-run values per condition. Aggregates measured values
/// only; the optional reduction figure is derived at report time from measured
/// modelInputTokens and is labeled as such.
export function summarizeReplayEvidence(doc) {
  const byCondition = {};
  for (const run of doc.runs) {
    const entry = (byCondition[run.condition] ??= {
      runs: 0,
      sampleSize: 0,
      modelInputTokens: 0,
      modelOutputTokens: 0,
      retrievalOutputTokens: 0,
      toolCalls: 0,
      elapsedMs: 0,
      correct: 0,
      retrievalErrors: 0,
      staleEvidence: 0,
    });
    entry.runs += 1;
    entry.sampleSize += run.sampleSize;
    entry.modelInputTokens += run.modelInputTokens;
    entry.modelOutputTokens += run.modelOutputTokens;
    entry.retrievalOutputTokens += run.retrievalOutputTokens;
    entry.toolCalls += run.toolCalls;
    entry.elapsedMs += run.elapsedMs;
    entry.correct += run.taskCorrect ? 1 : 0;
    entry.retrievalErrors += run.retrievalErrors;
    entry.staleEvidence += run.staleEvidence;
  }
  const summary = {
    runId: doc.runId,
    operator: doc.operator,
    model: doc.fixed.model,
    revision: doc.fixed.revision,
    promptVersion: doc.fixed.promptVersion,
    budgets: doc.fixed.budgets,
    tasks: doc.fixed.tasks.map((task) => task.id),
    conditions: Object.fromEntries(
      Object.entries(byCondition).map(([condition, entry]) => [
        condition,
        {
          ...entry,
          correctRate: entry.correct / entry.runs,
        },
      ]),
    ),
  };
  const lexical = byCondition["lexical"] ?? byCondition["docs-code-lexical"];
  const baseline = byCondition["curated-docs-rg"];
  if (lexical && baseline) {
    // Derived at report time from measured totals; never imported as a claim.
    const baselineMean = baseline.modelInputTokens / baseline.runs;
    const lexicalMean = lexical.modelInputTokens / lexical.runs;
    summary.inputTokenReduction = {
      baseline: "curated-docs-rg",
      candidate: "lexical",
      baselineMeanInputTokens: baselineMean,
      candidateMeanInputTokens: lexicalMean,
      reductionFraction: baselineMean === 0 ? null : (baselineMean - lexicalMean) / baselineMean,
    };
  }
  return summary;
}
