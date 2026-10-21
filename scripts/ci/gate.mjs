#!/usr/bin/env node
// Stable aggregate CI gate. Runs with `always()` so it evaluates selection
// errors, expected job results, failures and cancellations even when a
// depended-on job did not run normally. A skipped selection means the full
// suite was required; a selection error fails the gate (the full suite still
// ran); required jobs must have succeeded; non-required jobs must have been
// skipped. Nothing here is a no-op success.
import { fileURLToPath } from "node:url";

export function evaluate(env) {
  const failures = [];
  const report = [];
  const selectResult = env.GATE_SELECT_RESULT ?? "";
  let requireCheck;
  let requireDocs;
  if (selectResult === "skipped" || selectResult === "") {
    requireCheck = true;
    requireDocs = true;
    report.push("selection skipped; full suite required");
  } else {
    if (env.GATE_SELECT_OK !== "true") {
      failures.push(
        `selection reported an error (ok=${env.GATE_SELECT_OK ?? "missing"}); full suite ran, gate fails on selection errors`,
      );
    }
    if (env.GATE_RUN_CHECK === undefined || env.GATE_RUN_DOCS === undefined) {
      failures.push("selection outputs run_check/run_docs are missing");
      requireCheck = true;
      requireDocs = true;
    } else {
      requireCheck = env.GATE_RUN_CHECK === "true";
      requireDocs = env.GATE_RUN_DOCS === "true";
    }
  }
  const jobs = [
    ["Check (ubuntu-24.04)", env.GATE_CHECK_LINUX, requireCheck],
    ["Check (macos-15)", env.GATE_CHECK_MACOS, requireCheck],
    ["Check (windows-2025)", env.GATE_CHECK_WINDOWS, requireCheck],
    ["Docs", env.GATE_DOCS, requireDocs],
  ];
  for (const [name, result, required] of jobs) {
    if (required) {
      if (result === "success") {
        report.push(`${name}: success (required)`);
      } else {
        failures.push(
          `${name}: required but result was ${result || "missing"}`,
        );
      }
    } else {
      if (result === "skipped") {
        report.push(`${name}: skipped (not required)`);
      } else {
        failures.push(
          `${name}: not required by selection but result was ${result || "missing"}`,
        );
      }
    }
  }
  return { ok: failures.length === 0, failures, report };
}

function main() {
  const result = evaluate(process.env);
  for (const line of result.report) console.log(`[gate] ${line}`);
  for (const line of result.failures) console.error(`[gate] FAIL: ${line}`);
  if (result.ok) {
    console.log("[gate] aggregate gate passed");
    process.exit(0);
  }
  console.error(
    `[gate] aggregate gate failed: ${result.failures.length} problem(s)`,
  );
  process.exit(1);
}

if (process.argv[1] === fileURLToPath(import.meta.url)) main();
