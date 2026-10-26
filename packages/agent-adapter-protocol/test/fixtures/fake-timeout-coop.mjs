#!/usr/bin/env node
// Cooperative adapter: on the stdin `cancel` control line or SIGINT it emits
// a cancelled final and exits. Under a host wall clock the host's timeout
// must not be replaced by the cooperative cancelled final.
import { emit, hello, readRequest, watchControlLines } from "./helpers.mjs";

const request = await readRequest();
hello();
emit({
  v: 1,
  t: "started",
  ts: Date.now(),
  trialId: request?.identity?.trialId ?? "trial-1",
  workspace: request?.workspace ?? {},
});
const cancel = () => {
  emit({
    v: 1,
    t: "final",
    ts: Date.now(),
    status: "cancelled",
    reason: "host_requested",
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
  process.exit(0);
};
watchControlLines((line) => {
  if (line === "cancel") cancel();
});
process.on("SIGINT", cancel);
setInterval(() => {}, 1_000_000);
await new Promise(() => {});
