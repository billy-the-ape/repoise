#!/usr/bin/env node
// Emits hello and then never emits anything again and never reacts to
// signals: the host must hit the wall clock, escalate and report timeout.
import { hello, readRequest } from "./helpers.mjs";

await readRequest();
hello();
setInterval(() => {}, 1_000_000); // Stall forever.
await new Promise(() => {});
