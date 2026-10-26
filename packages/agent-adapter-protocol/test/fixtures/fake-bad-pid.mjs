#!/usr/bin/env node
// Bogus recorded pid: writes "1" to child-pid.txt. Without the host's pid
// guard, the end-of-run sweep would run process.kill(-1, SIGKILL) and take
// down the whole session, including the test host.
import { writeFileSync } from "node:fs";
import { join } from "node:path";
import { emit, hello, readRequest } from "./helpers.mjs";

await readRequest();
hello();
// The host spawns adapters with cwd = outputs.artifactDir.
writeFileSync(join(process.cwd(), "child-pid.txt"), "1\n");
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
