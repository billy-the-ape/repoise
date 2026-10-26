#!/usr/bin/env node
// Emits an event line that exceeds the request's maxEventBytes.
import { hello, readRequest } from "./helpers.mjs";

const request = await readRequest();
hello();
const maxBytes = request?.limits?.maxEventBytes ?? 65536;
const payload = "x".repeat(maxBytes * 2);
process.stdout.write(
  `${JSON.stringify({ v: 1, t: "note", ts: Date.now(), code: "big", message: payload })}\n`,
);
process.exit(0);
