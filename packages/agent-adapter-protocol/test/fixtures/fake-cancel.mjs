#!/usr/bin/env node
// Reacts to host cancellation: the stdin `cancel` control line (the
// cross-platform cooperative channel) or SIGINT makes it emit a final
// cancelled event and exit cleanly within the grace period, so the host
// never needs to escalate.
import { emit, hello, readRequest, watchControlLines } from "./helpers.mjs";

await readRequest();
hello();
const cancel = () => {
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
};
watchControlLines((line) => {
  if (line === "cancel") cancel();
});
process.on("SIGINT", cancel);
setInterval(() => {}, 1_000_000);
await new Promise(() => {});
