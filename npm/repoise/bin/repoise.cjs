#!/usr/bin/env node
const { spawn } = require("node:child_process");
const { binaryPath } = require("./platform.cjs");

try {
  const binary = binaryPath(
    process.platform,
    process.arch,
    process.platform === "linux" ? process.report.getReport() : undefined,
  );
  const child = spawn(binary, process.argv.slice(2), {
    stdio: "inherit",
    shell: false,
  });
  const signals = ["SIGINT", "SIGTERM"];
  const handlers = signals.map((signal) => () => child.kill(signal));
  signals.forEach((signal, index) => process.on(signal, handlers[index]));
  child.on("error", (error) => {
    console.error(
      `repoise: unable to start native executable: ${error.message}`,
    );
    process.exitCode = 1;
  });
  child.on("exit", (code, signal) => {
    signals.forEach((item, index) =>
      process.removeListener(item, handlers[index]),
    );
    if (signal) process.kill(process.pid, signal);
    else process.exitCode = code ?? 1;
  });
} catch (error) {
  console.error(`repoise: ${error.message}`);
  process.exitCode = 1;
}
