#!/usr/bin/env node
// Echoes the received `task.prompt` verbatim to `echo-prompt.txt` in the
// artifact directory: verifies that the request text the adapter receives
// is exactly what the coordinator sent (and recorded in requestHash), even
// for large non-ASCII prompts.
import { writeFileSync } from "node:fs";
import { join } from "node:path";
import { emit, hello, readRequest } from "./helpers.mjs";

const request = await readRequest();
hello();
// The host spawns adapters with cwd = outputs.artifactDir.
writeFileSync(
  join(process.cwd(), "echo-prompt.txt"),
  request?.task?.prompt ?? "",
);
emit({
  v: 1,
  t: "final",
  ts: Date.now(),
  status: "completed",
  reason: null,
  usage: {
    inputTokens: 1,
    outputTokens: 1,
    reasoningTokens: null,
    cacheReadTokens: null,
    cacheWriteTokens: null,
    costUsd: null,
  },
  durationMs: 1,
  exitCode: 0,
});
process.exit(0);
