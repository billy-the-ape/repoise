#!/usr/bin/env node
// Protocol violation: a valid final followed by another event, then exit.
// The host must report events_after_final, not a clean result.
import { emit, hello, readRequest } from "./helpers.mjs";

const request = await readRequest();
hello();
emit({
  v: 1,
  t: "started",
  ts: Date.now(),
  trialId: request?.identity?.trialId ?? "trial-1",
  workspace: request?.workspace ?? {},
});
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
  durationMs: 10,
  exitCode: 0,
});
emit({
  v: 1,
  t: "note",
  ts: Date.now(),
  code: "trailing",
  message: "should never be accepted",
});
process.exit(0);
