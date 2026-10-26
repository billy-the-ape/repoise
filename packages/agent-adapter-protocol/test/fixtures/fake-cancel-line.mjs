#!/usr/bin/env node
// Exercises the cooperative stdin channel alone: it deliberately ignores
// SIGINT (on Windows signals cannot be delivered to console-less Node
// processes at all) and reacts only to the host's `cancel` control line.
import { emit, hello, readRequest, watchControlLines } from "./helpers.mjs";

await readRequest();
hello();
watchControlLines((line) => {
  if (line === "cancel") {
    emit({
      v: 1,
      t: "final",
      ts: Date.now(),
      status: "cancelled",
      reason: "host_requested",
      usage: null,
      durationMs: 1,
      exitCode: 0,
    });
    process.exit(0);
  }
});
process.on("SIGINT", () => {}); // Deliberately ignored.
setInterval(() => {}, 1_000_000);
await new Promise(() => {});
