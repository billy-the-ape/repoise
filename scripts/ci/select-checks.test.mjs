// Tests for affected-check selection: NUL diff parsing (renames, deletions,
// paths with spaces), impact-map classification fixtures, and end-to-end
// selection against a temporary Git repository.
import test from "node:test";
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import {
  mkdirSync,
  writeFileSync,
  rmSync,
  readFileSync,
  mkdtempSync,
  copyFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { parseNameStatusZ, classifyPaths } from "./select-checks.mjs";

const here = dirname(fileURLToPath(import.meta.url));
const map = JSON.parse(readFileSync(join(here, "impact-map.json"), "utf8"));

const entry = (status, path) => ({
  status,
  oldPath: path,
  newPath: path,
});

test("parses NUL name-status output including renames and spaces", () => {
  const output = [
    "M\0crates/repoise-core/src/a.rs",
    "A\0new dir/file with spaces.js",
    "D\0gone/doc.md",
    "T\0links/README.md",
    "R100\0old name.rs\0new name.rs",
    "C066\0src/orig.ts\0src/copy.ts",
    "",
  ].join("\0");
  const entries = parseNameStatusZ(output);
  assert.deepEqual(entries, [
    {
      status: "M",
      oldPath: "crates/repoise-core/src/a.rs",
      newPath: "crates/repoise-core/src/a.rs",
    },
    {
      status: "A",
      oldPath: "new dir/file with spaces.js",
      newPath: "new dir/file with spaces.js",
    },
    { status: "D", oldPath: "gone/doc.md", newPath: "gone/doc.md" },
    { status: "T", oldPath: "links/README.md", newPath: "links/README.md" },
    { status: "R", oldPath: "old name.rs", newPath: "new name.rs" },
    { status: "C", oldPath: "src/orig.ts", newPath: "src/copy.ts" },
  ]);
  assert.throws(() => parseNameStatusZ("X\0broken\0"));
});

test("type changes classify like modifications", () => {
  const result = classifyPaths(
    [entry("T", "crates/repoise-core/src/main.rs")],
    map,
  );
  assert.equal(result.full, false);
  assert.deepEqual(result.jobs, ["check"]);
  assert.deepEqual([...result.components.keys()], ["core"]);
});

test("core changes select only the Check jobs", () => {
  const result = classifyPaths(
    [entry("M", "crates/repoise-core/src/main.rs")],
    map,
  );
  assert.equal(result.full, false);
  assert.deepEqual(result.jobs, ["check"]);
  assert.deepEqual([...result.components.keys()], ["core"]);
});

test("cli, launcher, release, eval and schema changes select the Check jobs", () => {
  for (const path of [
    "crates/repoise-cli/src/main.rs",
    "npm/repoise/bin/repoise.cjs",
    "scripts/release/pack.mjs",
    "scripts/eval/harness.mjs",
    "schemas/repoise.config.v1.schema.json",
  ]) {
    const result = classifyPaths([entry("M", path)], map);
    assert.deepEqual(result.jobs, ["check"], path);
    assert.equal(result.full, false, path);
  }
});

test("agent adapter packages and the reserved extension home select broad checks", () => {
  for (const path of [
    "packages/agent-adapter-cline/src/runner.ts",
    "packages/agent-adapter-protocol/src/index.js",
    "apps/vscode/package.json",
  ]) {
    const result = classifyPaths([entry("A", path)], map);
    assert.deepEqual(result.jobs, ["check"], path);
  }
});

test("docs-only changes select only the Docs job", () => {
  for (const path of [
    "docs/development.md",
    "README.md",
    "AGENTS.md",
    "CHANGELOG.md",
    "skills/repoise/SKILL.md",
    "examples/ci/repoise-ci.example.yml",
  ]) {
    const result = classifyPaths([entry("M", path)], map);
    assert.deepEqual(result.jobs, ["docs"], path);
    assert.equal(result.full, false, path);
  }
});

test("lockfile, toolchain, root config and workflow changes select the full suite", () => {
  for (const path of [
    "pnpm-lock.yaml",
    "pnpm-workspace.yaml",
    "turbo.json",
    "package.json",
    "Cargo.toml",
    "Cargo.lock",
    "rust-toolchain.toml",
    ".github/workflows/ci.yml",
    "scripts/ci/impact-map.json",
    "scripts/check.sh",
  ]) {
    const result = classifyPaths([entry("M", path)], map);
    assert.deepEqual([...result.jobs].sort(), ["check", "docs"], path);
    assert.equal(result.full, false, path);
  }
});

test("unknown paths select the full suite", () => {
  const result = classifyPaths([entry("A", "mystery/unknown.txt")], map);
  assert.equal(result.full, true);
  assert.deepEqual([...result.jobs].sort(), ["check", "docs"]);
  assert.deepEqual(result.unknown, ["mystery/unknown.txt"]);
});

test("renames consider both sides; deletion uses the old path", () => {
  const renamed = classifyPaths(
    [
      {
        status: "R",
        oldPath: "docs/moved.md",
        newPath: "crates/repoise-core/src/moved.md",
      },
    ],
    map,
  );
  assert.equal(renamed.full, false);
  assert.deepEqual([...renamed.jobs].sort(), ["check", "docs"]);
  const deleted = classifyPaths([entry("D", "docs/removed.md")], map);
  assert.deepEqual(deleted.jobs, ["docs"]);
  const unknownRename = classifyPaths(
    [
      {
        status: "R",
        oldPath: "spaced old.txt",
        newPath: "spaced dir/new name.txt",
      },
    ],
    map,
  );
  assert.equal(unknownRename.full, true);
});

test("an empty diff selects the full suite", () => {
  const result = classifyPaths([], map);
  assert.equal(result.full, true);
  assert.deepEqual([...result.jobs].sort(), ["check", "docs"]);
  assert.match(result.reason, /empty diff/);
});

function git(repo, args) {
  const result = spawnSync("git", args, {
    cwd: repo,
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
  });
  if (result.status !== 0)
    throw new Error(`git ${args.join(" ")}: ${result.stderr}`);
  return result.stdout;
}

function makeRepo() {
  const repo = mkdtempSync(join(tmpdir(), "repoise-select-"));
  git(repo, ["init", "-b", "main"]);
  git(repo, ["config", "user.name", "Select Test"]);
  git(repo, ["config", "user.email", "select@test.local"]);
  mkdirSync(join(repo, "crates/repoise-core/src"), { recursive: true });
  mkdirSync(join(repo, "docs"), { recursive: true });
  mkdirSync(join(repo, "scripts/ci"), { recursive: true });
  writeFileSync(
    join(repo, "crates/repoise-core/src/main.rs"),
    "fn main() {}\n",
  );
  writeFileSync(join(repo, "docs/guide.md"), "# guide\n");
  writeFileSync(join(repo, "spaced name.txt"), "hello\n");
  copyFileSync(
    join(here, "impact-map.json"),
    join(repo, "scripts/ci/impact-map.json"),
  );
  git(repo, ["add", "-A"]);
  git(repo, ["commit", "-m", "initial"]);
  return repo;
}

function commitAll(repo, message) {
  git(repo, ["add", "-A"]);
  git(repo, ["commit", "-m", message]);
  return git(repo, ["rev-parse", "HEAD"]).trim();
}

function runSelect(repo, base, head) {
  const outFile = mkdtempSync(join(tmpdir(), "repoise-select-out-"));
  const output = join(outFile, "github-output");
  const result = spawnSync(
    process.execPath,
    [
      join(here, "select-checks.mjs"),
      "--repo",
      repo,
      "--base",
      base,
      "--head",
      head,
    ],
    {
      encoding: "utf8",
      env: { ...process.env, GITHUB_OUTPUT: output },
    },
  );
  const values = {};
  if (readFileSyncSafe(output)) {
    for (const line of readFileSync(output, "utf8").split("\n")) {
      const idx = line.indexOf("=");
      if (idx > 0) values[line.slice(0, idx)] = line.slice(idx + 1);
    }
  }
  return { code: result.status, values, stdout: result.stdout };
}

function readFileSyncSafe(path) {
  try {
    readFileSync(path);
    return true;
  } catch {
    return false;
  }
}

test("selects docs-only for a docs-only PR diff", () => {
  const repo = makeRepo();
  const base = git(repo, ["rev-parse", "HEAD"]).trim();
  writeFileSync(join(repo, "docs/guide.md"), "# guide v2\n");
  const head = commitAll(repo, "docs change");
  const result = runSelect(repo, base, head);
  rmSync(repo, { recursive: true, force: true });
  assert.equal(result.code, 0);
  assert.equal(result.values.ok, "true");
  assert.equal(result.values.run_check, "false");
  assert.equal(result.values.run_docs, "true");
  assert.equal(result.values.components, "docs");
});

test("full suite for rename, deletion and unknown spaced paths", () => {
  const repo = makeRepo();
  const base = git(repo, ["rev-parse", "HEAD"]).trim();
  writeFileSync(
    join(repo, "crates/repoise-core/src/main.rs"),
    "fn main() { let x = 1; }\n",
  );
  mkdirSync(join(repo, "crates/repoise-cli"), { recursive: true });
  git(repo, ["mv", "docs/guide.md", "crates/repoise-cli/guide.md"]);
  mkdirSync(join(repo, "spaced dir"), { recursive: true });
  git(repo, ["mv", "spaced name.txt", "spaced dir/moved file.txt"]);
  const head = commitAll(repo, "cross changes");
  const result = runSelect(repo, base, head);
  rmSync(repo, { recursive: true, force: true });
  assert.equal(result.code, 0);
  assert.equal(result.values.ok, "true");
  assert.equal(result.values.run_check, "true");
  assert.equal(result.values.run_docs, "true");
  assert.equal(result.values.components, "full");
  assert.match(result.values.reason, /unknown path/);
});

test("a missing base fails the selection and selects the full suite", () => {
  const repo = makeRepo();
  const head = git(repo, ["rev-parse", "HEAD"]).trim();
  const result = runSelect(repo, "refs/heads/does-not-exist", head);
  rmSync(repo, { recursive: true, force: true });
  assert.equal(result.code, 1);
  assert.equal(result.values.ok, "false");
  assert.equal(result.values.run_check, "true");
  assert.equal(result.values.run_docs, "true");
  assert.equal(result.values.components, "full");
});

test("an empty diff (base equals head) selects the full suite", () => {
  const repo = makeRepo();
  const head = git(repo, ["rev-parse", "HEAD"]).trim();
  const result = runSelect(repo, head, head);
  rmSync(repo, { recursive: true, force: true });
  assert.equal(result.code, 0);
  assert.equal(result.values.ok, "true");
  assert.equal(result.values.run_check, "true");
  assert.equal(result.values.run_docs, "true");
  assert.match(result.values.reason, /empty diff/);
});
