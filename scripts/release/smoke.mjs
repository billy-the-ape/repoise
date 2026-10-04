import assert from "node:assert/strict";
import {
  mkdtempSync,
  mkdirSync,
  writeFileSync,
  rmSync,
  readdirSync,
  readFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { spawnSync } from "node:child_process";
import { root, targets, manifest, npm, run } from "./common.mjs";
const spec = targets.find((item) => item.target === process.argv[2]);
if (!spec) throw new Error("Pass a supported target");
const directory = resolve(root, process.argv[3] ?? `dist/${spec.target}`);
const temporary = mkdtempSync(join(tmpdir(), "repoise smoke "));
try {
  const consumer = join(temporary, "consumer");
  mkdirSync(consumer);
  writeFileSync(
    join(consumer, "package.json"),
    JSON.stringify({ name: "release-consumer", private: true }),
  );
  npm([
    "pack",
    join(root, "npm/repoise"),
    "--json",
    "--ignore-scripts",
    "--pack-destination",
    temporary,
  ]);
  const wrapper = join(temporary, `repoise-${manifest().version}.tgz`);
  const native = join(directory, `${spec.package}-${manifest().version}.tgz`);
  npm(
    [
      "install",
      "--ignore-scripts",
      "--no-audit",
      "--no-fund",
      "--offline",
      "--omit=optional",
      native,
      wrapper,
    ],
    { cwd: consumer },
  );
  const launch = join(consumer, "node_modules/repoise/bin/repoise.cjs");
  assert.equal(
    run(process.execPath, [launch, "--version"], { cwd: consumer }),
    `repoise ${manifest().version}`,
  );
  assert.match(
    run(process.execPath, [launch, "--help"], { cwd: consumer }),
    /Usage: repoise/,
  );
  const invalid = spawnSync(process.execPath, [launch, "--unknown"], {
    cwd: consumer,
    encoding: "utf8",
  });
  assert.equal(invalid.status, 2);
  assert.equal(invalid.stdout, "");
  assert.match(invalid.stderr, /--help/);
  const unpacked = join(temporary, "archive");
  mkdirSync(unpacked);
  const archive = readdirSync(directory).find((name) =>
    name.endsWith(".tar.gz"),
  );
  run("tar", ["-xzf", join(directory, archive), "-C", unpacked]);
  const executable = join(unpacked, spec.binary);
  const nativeResult = spawnSync(executable, ["--version"], {
    cwd: consumer,
    env: { ...process.env, PATH: "" },
    encoding: "utf8",
  });
  assert.equal(nativeResult.status, 0, nativeResult.stderr);
  assert.equal(nativeResult.stdout.trim(), `repoise ${manifest().version}`);
  assert.equal(
    readFileSync(join(unpacked, "LICENSE"), "utf8"),
    readFileSync(join(root, "LICENSE"), "utf8"),
  );
  console.log(
    "Packed launcher and native archive smoke passed (offline consumer, no install scripts, no Node on native PATH).",
  );
} finally {
  rmSync(temporary, { recursive: true, force: true });
}
