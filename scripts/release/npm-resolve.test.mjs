// Tests that release tooling resolves the real npm CLI even when a package
// manager such as pnpm points npm_execpath at its own CLI.
import test from "node:test";
import assert from "node:assert/strict";
import {
  mkdirSync,
  writeFileSync,
  symlinkSync,
  mkdtempSync,
  rmSync,
} from "node:fs";
import { join } from "node:path";
import { resolveNpmCli } from "./common.mjs";

test("rejects a pnpm execpath and falls back to the PATH npm shim", () => {
  const dir = mkdtempSync(join(process.cwd(), "npm-resolve-"));
  try {
    const bin = join(dir, "bin");
    mkdirSync(bin, { recursive: true });
    const pnpmCli = join(bin, "pnpm-cli.js");
    writeFileSync(pnpmCli, "#!/usr/bin/env node\n");
    const npmCli = join(bin, "node_modules/npm/bin/npm-cli.js");
    mkdirSync(join(dir, "bin/node_modules/npm/bin"), { recursive: true });
    writeFileSync(npmCli, "#!/usr/bin/env node\n");
    // Real Node installs ship `npm` as a symlink to npm-cli.js.
    symlinkSync(npmCli, join(bin, "npm"));
    const resolved = resolveNpmCli(
      { npm_execpath: pnpmCli, PATH: bin },
      "linux",
    );
    assert.equal(resolved, npmCli);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("accepts an npm_execpath that really is npm-cli.js", () => {
  const dir = mkdtempSync(join(process.cwd(), "npm-resolve-"));
  try {
    const npmCli = join(dir, "node_modules/npm/bin/npm-cli.js");
    mkdirSync(join(dir, "node_modules/npm/bin"), { recursive: true });
    writeFileSync(npmCli, "#!/usr/bin/env node\n");
    const resolved = resolveNpmCli({ npm_execpath: npmCli }, "linux");
    assert.equal(resolved, npmCli);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("maps the Windows npm.cmd shim to npm-cli.js", () => {
  const dir = mkdtempSync(join(process.cwd(), "npm-resolve-"));
  try {
    const bin = join(dir, "bin");
    const npmCli = join(bin, "node_modules/npm/bin/npm-cli.js");
    mkdirSync(join(bin, "node_modules/npm/bin"), { recursive: true });
    writeFileSync(npmCli, "#!/usr/bin/env node\n");
    writeFileSync(join(bin, "npm.cmd"), "@echo off\n");
    const resolved = resolveNpmCli({ PATH: bin }, "win32");
    assert.equal(resolved, npmCli);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("fails when no npm CLI is available", () => {
  const dir = mkdtempSync(join(process.cwd(), "npm-resolve-"));
  try {
    const empty = join(dir, "empty");
    mkdirSync(empty);
    assert.throws(
      () => resolveNpmCli({ PATH: empty }, "linux"),
      /npm CLI not found/,
    );
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});
