#!/usr/bin/env node
// A leaked grandchild: appends to HB_FILE every 50 ms and ignores SIGINT.
// Only SIGKILL (host escalation of the owned process tree) stops it.
import { appendFileSync } from "node:fs";

process.on("SIGINT", () => {});
const file = process.env.HB_FILE;
setInterval(() => {
  try {
    appendFileSync(file, "x");
  } catch {
    // The heartbeat file may be removed between appends; keep running.
  }
}, 50);
await new Promise(() => {});
