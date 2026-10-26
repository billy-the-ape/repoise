#!/usr/bin/env node
// Prints a single blank line, then stalls: a blank first line must not
// disarm the hello timeout.
process.stdout.write("\n");
process.on("SIGINT", () => {});
setInterval(() => {}, 1_000_000); // Stall forever.
await new Promise(() => {});
