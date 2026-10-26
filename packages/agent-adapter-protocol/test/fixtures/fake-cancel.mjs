#!/usr/bin/env node
// Reacts to SIGINT: emits a final cancelled event and exits cleanly within
// the grace period, so the host never needs to escalate.
import { emit, hello, readRequest } from "./helpers.mjs";

await readRequest();
hello();
process.on("SIGINT", () => {
  emit({ v: 1, t: "final", ts: Date.now(), status: "cancelled", reason: "signal", usage: null, durationMs: 1, exitCode: 0 });
  process.exit(0);
});
setInterval(() => {}, 1_000_000);
await new Promise(() => {});
