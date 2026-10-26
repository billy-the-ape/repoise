#!/usr/bin/env node
// Emits one valid note, then a line that is not valid JSON.
import { emit, hello, readRequest } from "./helpers.mjs";

await readRequest();
hello();
emit({
  v: 1,
  t: "note",
  ts: Date.now(),
  code: "ok",
  message: "before malformed",
});
process.stdout.write('{"v":1,"t":"note","ts":1,"code":"broken",\n');
process.exit(0);
