#!/usr/bin/env node
// The adapter emits a valid final and exits 0, but leaves a SIGINT-ignoring
// grandchild behind: the host must sweep the owned process group after the
// clean exit so the grandchild cannot outlive the trial.
import { spawn } from "node:child_process";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { emit, hello, readRequest } from "./helpers.mjs";

const request = await readRequest();
hello();
emit({ v: 1, t: "started", ts: Date.now(), trialId: request?.identity?.trialId ?? "trial-1", workspace: request?.workspace ?? {} });
const heartbeat = join(dirname(fileURLToPath(import.meta.url)), "heartbeat.mjs");
const grandchild = spawn(process.execPath, [heartbeat], {
  env: { ...process.env, HB_FILE: process.env.LEAK_HB_FILE },
  stdio: ["ignore", "ignore", "ignore"],
});
void grandchild;
emit({
  v: 1,
  t: "final",
  ts: Date.now(),
  status: "completed",
  reason: null,
  usage: { inputTokens: 1, outputTokens: 1, reasoningTokens: null, cacheReadTokens: null, cacheWriteTokens: null, costUsd: null },
  durationMs: 5,
  exitCode: 0,
});
process.exit(0);
