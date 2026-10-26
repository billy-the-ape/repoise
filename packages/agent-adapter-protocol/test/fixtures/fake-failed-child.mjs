#!/usr/bin/env node
// Its own child fails and the adapter dies without emitting a final event:
// the host must report adapter_error / nonzero_exit.
import { spawn } from "node:child_process";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { hello, readRequest } from "./helpers.mjs";

await readRequest();
hello();
const child = join(dirname(fileURLToPath(import.meta.url)), "child.mjs");
const code = await new Promise((resolve) => {
  spawn(process.execPath, [child], { stdio: "ignore" }).once("close", (c) =>
    resolve(c),
  );
});
process.exit(code ?? 1);
