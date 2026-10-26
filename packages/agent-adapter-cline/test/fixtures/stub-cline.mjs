#!/usr/bin/env node
// Stub Cline CLI for adapter tests: reproduces the documented headless
// NDJSON surface without any model work. Modes (STUB_MODE): ok (default),
// fail (stderr error + exit 1), hang (stays until SIGINT, then aborts).
import { writeFileSync } from "node:fs";
import { join } from "node:path";

function emit(obj) {
  process.stdout.write(
    JSON.stringify({ ts: new Date().toISOString(), ...obj }) + "\n",
  );
}

if (process.argv[2] === "version") {
  process.stdout.write("1.0.0-stub\n");
  process.exit(0);
}

const mode = process.env.CLONE_STUB_MODE ?? process.env.STUB_MODE ?? "ok";

const prompt = await new Promise((resolve) => {
  let data = "";
  process.stdin.setEncoding("utf8");
  process.stdin.on("data", (chunk) => (data += chunk));
  process.stdin.on("end", () => resolve(data));
});

// The adapter passes a minimal environment, so the stub records what it
// observed in the workspace cwd (the adapter sets cwd to workspace.root).
writeFileSync(
  join(process.cwd(), "stub-manifest.json"),
  JSON.stringify(
    {
      argv: process.argv.slice(2),
      prompt,
      mcpSettingsPath: process.env.CLINE_MCP_SETTINGS_PATH,
      logPath: process.env.CLONE_LOG_PATH,
      cwd: process.cwd(),
      envNames: Object.keys(process.env),
    },
    null,
    2,
  ),
);

if (process.env.CLONE_ECHO_KEY === "1" && process.env.CLINE_TEST_KEY) {
  // Credential-scrubbing test: echo the key on both stdout and stderr the
  // way a real CLI would in an invalid-key error.
  const key = process.env.CLINE_TEST_KEY;
  process.stdout.write(`echoed secret ${key} on stdout\n`);
  // Unparseable line whose key straddles the 200-char prefix boundary of
  // cline_unparsed_line: redaction must happen before truncation.
  process.stdout.write(`${"A".repeat(190)}${key} tail\n`);
  process.stderr.write(
    `${JSON.stringify({ type: "error", message: `invalid key ${key}` })}\n`,
  );
}

if (mode === "fail") {
  process.stderr.write(
    `${JSON.stringify({ type: "error", message: "stub failure" })}\n`,
  );
  process.exit(1);
}
if (mode === "hang") {
  emit({ type: "run_start" });
  process.on("SIGINT", () => {
    emit({
      type: "run_aborted",
      reason: "cancelled",
      message: "stub cancelled",
    });
    emit({
      type: "run_result",
      finishReason: "aborted",
      iterations: 1,
      usage: { tokensIn: 5, tokensOut: 7, costUsd: 0.001 },
      durationMs: 12,
      model: "stub-model",
      text: "interrupted",
    });
    process.exit(0);
  });
  setInterval(() => {}, 1_000_000);
  await new Promise(() => {});
}

emit({ type: "run_start" });
emit({ type: "agent_event", event: { type: "iteration_start" } });
emit({
  type: "agent_event",
  event: { type: "usage", tokensIn: 10, tokensOut: 25, costUsd: 0.003 },
});
emit({
  type: "agent_event",
  event: { type: "iteration_end", iteration: { durationMs: 41 } },
});
emit({
  type: "agent_event",
  event: { type: "done", reason: "completed", text: "stub answer" },
});
emit({
  type: "run_result",
  finishReason: "completed",
  iterations: 1,
  usage: { tokensIn: 10, tokensOut: 25, costUsd: 0.003 },
  durationMs: 42,
  model: "stub-model",
  text: "stub answer",
});
process.exit(0);
