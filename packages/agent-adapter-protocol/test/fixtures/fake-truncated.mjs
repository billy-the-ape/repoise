#!/usr/bin/env node
// Emits one valid note, then a partial line with no terminating newline
// before exiting: a truncation, not a silent drop.
import { emit, hello, readRequest } from "./helpers.mjs";

await readRequest();
hello();
emit({ v: 1, t: "note", ts: Date.now(), code: "ok", message: "before truncation" });
process.stdout.write('{"v":1,"t":"note","ts":2,"code":"truncated"');
process.exit(0);
