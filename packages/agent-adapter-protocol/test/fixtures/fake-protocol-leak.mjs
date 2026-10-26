#!/usr/bin/env node
// Protocol-violation leak: records a heartbeat grandchild in its own
// process group in `child-pid.txt` (where the host looks), then emits an
// oversized line. The host must failProtocol and sweep the recorded child
// group - its own group sweep cannot reach a detached grandchild.
import { spawn } from "node:child_process";
import { writeFileSync } from "node:fs";
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
  env: { ...process.env, HB_FILE: process.env.PROTO_LEAK_HB_FILE },
  stdio: ["ignore", "ignore", "ignore"],
  detached: true,
});
grandchild.unref();
// The host spawns adapters with cwd = outputs.artifactDir.
writeFileSync(join(process.cwd(), "child-pid.txt"), `${grandchild.pid}\n`);
// Let the heartbeat write a few ticks before the violation fires.
await new Promise((resolve) => setTimeout(resolve, 200));
// Oversized line: exceeds the host's maxEventBytes (set small by the test).
process.stdout.write(`${"A".repeat(4000)}\n`);
await new Promise(() => {});
