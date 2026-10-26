#!/usr/bin/env node
// Successful run: delta usage events plus an authoritative final total.
import { emit, hello, readRequest } from "./helpers.mjs";

const request = await readRequest();
hello();
emit({ v: 1, t: "started", ts: Date.now(), trialId: request?.identity?.trialId ?? "trial-1", workspace: request?.workspace ?? {} });
emit({ v: 1, t: "model_request_start", ts: Date.now(), requestId: "m1" });
emit({
  v: 1,
  t: "usage",
  ts: Date.now(),
  values: { inputTokens: 10, outputTokens: 20, reasoningTokens: null, cacheReadTokens: null, cacheWriteTokens: null, costUsd: 0.01 },
  raw: { provider: "fake" },
  source: "provider",
});
emit({
  v: 1,
  t: "usage",
  ts: Date.now(),
  values: { inputTokens: 5, outputTokens: 5, reasoningTokens: null, cacheReadTokens: null, cacheWriteTokens: null, costUsd: 0.005 },
  raw: { provider: "fake" },
  source: "provider",
});
emit({ v: 1, t: "model_request_end", ts: Date.now(), requestId: "m1", durationMs: 80, ok: true });
emit({ v: 1, t: "artifact", ts: Date.now(), path: "result.json", kind: "result" });
emit({
  v: 1,
  t: "final",
  ts: Date.now(),
  status: "completed",
  reason: null,
  usage: { inputTokens: 15, outputTokens: 25, reasoningTokens: null, cacheReadTokens: null, cacheWriteTokens: null, costUsd: 0.015 },
  durationMs: 120,
  exitCode: 0,
});
process.exit(0);
