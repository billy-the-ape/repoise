#!/usr/bin/env node
// Protocol-violation leak: records a heartbeat grandchild in its own
// process group in `child-pid.txt` (where the host looks), then emits an
// oversized line. The host must failProtocol and sweep the recorded child
// group - its own group sweep cannot reach a detached grandchild.
import { spawn } from "node:child_process";
import { writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { hello, readRequest, waitForFileSize } from "./helpers.mjs";

await readRequest();
hello();
const heartbeatFile = process.env.PROTO_LEAK_HB_FILE;
if (!heartbeatFile) {
  console.error("fake-protocol-leak: PROTO_LEAK_HB_FILE is not set");
  process.exit(1);
}
const heartbeat = join(
  dirname(fileURLToPath(import.meta.url)),
  "heartbeat.mjs",
);
const grandchild = spawn(process.execPath, [heartbeat], {
  env: { ...process.env, HB_FILE: heartbeatFile },
  stdio: ["ignore", "ignore", "ignore"],
  detached: true,
});
grandchild.unref();
// The host spawns adapters with cwd = outputs.artifactDir. Record the pid
// before waiting on readiness so the host can sweep it on any failure path.
writeFileSync(join(process.cwd(), "child-pid.txt"), `${grandchild.pid}\n`);
// Explicit readiness handshake: the heartbeat's first successful write is
// the readiness signal. Wait (bounded) for it before firing the violation,
// so "heartbeat ran before the kill" is deterministic, not a timing bet.
try {
  await waitForFileSize(heartbeatFile, 1, 5000);
} catch (error) {
  console.error(`fake-protocol-leak: ${error.message}`);
  process.exit(1);
}
// Oversized line: exceeds the host's maxEventBytes (set small by the test).
process.stdout.write(`${"A".repeat(4000)}\n`);
await new Promise(() => {});
