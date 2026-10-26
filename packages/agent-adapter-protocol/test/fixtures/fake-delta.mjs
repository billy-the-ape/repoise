#!/usr/bin/env node
// Delta usage semantics: deltas sum; the final total is authoritative and must
// not be added to the summed deltas (no double counting).
import { emit, hello, readRequest } from "./helpers.mjs";

await readRequest();
hello();
emit({
  v: 1,
  t: "usage",
  ts: Date.now(),
  values: {
    inputTokens: 10,
    outputTokens: null,
    reasoningTokens: null,
    cacheReadTokens: null,
    cacheWriteTokens: null,
    costUsd: null,
  },
  source: "provider",
});
emit({
  v: 1,
  t: "usage",
  ts: Date.now(),
  values: {
    inputTokens: 20,
    outputTokens: null,
    reasoningTokens: null,
    cacheReadTokens: null,
    cacheWriteTokens: null,
    costUsd: null,
  },
  source: "provider",
});
emit({
  v: 1,
  t: "final",
  ts: Date.now(),
  status: "completed",
  reason: null,
  usage: {
    inputTokens: 30,
    outputTokens: null,
    reasoningTokens: null,
    cacheReadTokens: null,
    cacheWriteTokens: null,
    costUsd: null,
  },
  durationMs: 50,
  exitCode: 0,
});
process.exit(0);
