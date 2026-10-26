#!/usr/bin/env node
// Cumulative usage semantics: snapshots grow; the final total must not be
// added to the snapshots (the host must take max, not sum).
import { emit, hello, readRequest } from "./helpers.mjs";

await readRequest();
hello({
  usage: {
    eventSemantics: "cumulative",
    inputIncludesCacheReads: null,
    includesSubagents: null,
    includesCompaction: null,
    costSemantics: "provider",
  },
});
emit({ v: 1, t: "usage", ts: Date.now(), values: { inputTokens: 10, outputTokens: null, reasoningTokens: null, cacheReadTokens: null, cacheWriteTokens: null, costUsd: 0.01 }, source: "provider" });
emit({ v: 1, t: "usage", ts: Date.now(), values: { inputTokens: 30, outputTokens: null, reasoningTokens: null, cacheReadTokens: null, cacheWriteTokens: null, costUsd: 0.03 }, source: "provider" });
emit({ v: 1, t: "final", ts: Date.now(), status: "completed", reason: null, usage: { inputTokens: 30, outputTokens: null, reasoningTokens: null, cacheReadTokens: null, cacheWriteTokens: null, costUsd: 0.03 }, durationMs: 50, exitCode: 0 });
process.exit(0);
