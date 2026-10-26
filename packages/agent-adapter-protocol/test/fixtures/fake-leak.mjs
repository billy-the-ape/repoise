#!/usr/bin/env node
// Spawns a grandchild heartbeat writer, then ignores SIGINT: the host must
// escalate to SIGKILL on the owned process tree so the grandchild dies too.
import { spawn } from "node:child_process";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { hello, readRequest } from "./helpers.mjs";

await readRequest();
hello();
const heartbeat = join(
  dirname(fileURLToPath(import.meta.url)),
  "heartbeat.mjs",
);
const grandchild = spawn(process.execPath, [heartbeat], {
  env: { ...process.env, HB_FILE: process.env.LEAK_HB_FILE },
  stdio: ["ignore", "ignore", "ignore"],
});
void grandchild;
process.on("SIGINT", () => {});
await new Promise(() => {});
