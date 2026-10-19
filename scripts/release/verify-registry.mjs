import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import {
  existsSync,
  mkdtempSync,
  mkdirSync,
  readFileSync,
  realpathSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { spawnSync } from "node:child_process";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";
import { verifyBundle } from "./bundle.mjs";
import { root, manifest, targets, npm, run } from "./common.mjs";

const REGISTRY = "https://registry.npmjs.org/";
const CLI_TIMEOUT_MS = 60_000;

/// Registry tarballs are identified by SHA-1 (`dist.shasum`); compute the
/// matching value for a reviewed bundle payload.
export function sha1Hex(file) {
  return createHash("sha1").update(readFileSync(file)).digest("hex");
}

/// `npm stage list --json` returns an array, or a version-keyed map in some
/// client versions; normalize both to an entry array.
export function stageEntries(raw) {
  const parsed = JSON.parse(raw);
  return Array.isArray(parsed) ? parsed : Object.values(parsed);
}

export function stagedVersion(entries, version) {
  return entries.filter((entry) => entry?.version === version);
}

/// Read-only registry verification against a reviewed release bundle.
/// Modes:
///   stage     one pending stage per package at the release version
///   published published tarballs/dist-tags/optional pins match the bundle
///   install   fresh pinned registry install in a temporary consumer
export function verifyRegistry(mode, bundlePath, sha) {
  const directory = resolve(bundlePath);
  const release = verifyBundle(directory, sha);
  if (mode === "stage") {
    for (const name of release.packages) {
      const raw = npm([
        "stage",
        "list",
        name,
        "--json",
        "--registry",
        REGISTRY,
      ]);
      const staged = stagedVersion(stageEntries(raw), release.version);
      assert.equal(
        staged.length,
        1,
        `${name}: expected exactly one pending stage at ${release.version}, found ${staged.length}`,
      );
      console.log(
        `${name}: pending stage ${staged[0].id ?? "(no id reported)"}`,
      );
    }
    console.log(
      "Staged state matches the reviewed bundle. Approve the three native stages with 2FA first, verify them, then approve the launcher stage.",
    );
    return;
  }
  if (mode === "published") {
    for (const name of release.packages) {
      const view = JSON.parse(
        npm([
          "view",
          `${name}@${release.version}`,
          "version",
          "dist.shasum",
          "--json",
          "--registry",
          REGISTRY,
        ]),
      );
      assert.equal(
        view.version,
        release.version,
        `${name}: wrong published version`,
      );
      assert.equal(
        view["dist.shasum"],
        sha1Hex(join(directory, `${name}-${release.version}.tgz`)),
        `${name}: published tarball differs from the reviewed bundle`,
      );
      const tags = JSON.parse(
        npm(["view", name, "dist-tags", "--json", "--registry", REGISTRY]),
      );
      assert.equal(
        tags[release.tag],
        release.version,
        `${name}: ${release.tag} does not resolve to ${release.version}`,
      );
    }
    const optional = JSON.parse(
      npm([
        "view",
        `${manifest().name}@${release.version}`,
        "optionalDependencies",
        "--json",
        "--registry",
        REGISTRY,
      ]),
    );
    for (const item of targets)
      assert.equal(
        optional[item.package],
        release.version,
        `${item.package}: launcher optional dependency pin mismatch`,
      );
    console.log(
      "Published registry state matches the reviewed bundle for all four packages.",
    );
    return;
  }
  const require = createRequire(import.meta.url);
  let expectedNative;
  try {
    const { packageFor } = require(join(root, "npm/repoise/bin/platform.cjs"));
    expectedNative = packageFor(
      process.platform,
      process.arch,
      process.platform === "linux" ? process.report.getReport() : undefined,
    );
  } catch {
    expectedNative = undefined;
  }
  const temporary = mkdtempSync(join(tmpdir(), "repoise registry install "));
  try {
    const consumer = join(temporary, "consumer");
    mkdirSync(consumer);
    writeFileSync(
      join(consumer, "package.json"),
      JSON.stringify({ name: "registry-consumer", private: true }),
    );
    npm(
      [
        "install",
        "--save-exact",
        "--ignore-scripts",
        "--no-audit",
        "--no-fund",
        `${manifest().name}@${release.version}`,
      ],
      { cwd: consumer, timeout: 300_000 },
    );
    if (!expectedNative) {
      assert.ok(
        existsSync(join(consumer, "node_modules/repoise")),
        "launcher did not install",
      );
      console.log(
        `Launcher installed on unsupported ${process.platform}/${process.arch}; functional CLI checks are not available there.`,
      );
      return;
    }
    assert.ok(
      existsSync(join(consumer, "node_modules", expectedNative)),
      `${expectedNative} optional dependency did not install`,
    );
    const launch = join(consumer, "node_modules/repoise/bin/repoise.cjs");
    assert.equal(
      run(process.execPath, [launch, "--version"], {
        cwd: consumer,
        timeout: CLI_TIMEOUT_MS,
      }),
      `repoise ${release.version}`,
    );
    assert.match(
      run(process.execPath, [launch, "--help"], {
        cwd: consumer,
        timeout: CLI_TIMEOUT_MS,
      }),
      /USAGE:\s+repoise/,
    );
    const invalid = spawnSync(process.execPath, [launch, "--unknown"], {
      cwd: consumer,
      encoding: "utf8",
      timeout: CLI_TIMEOUT_MS,
    });
    assert.equal(invalid.status, 2);
    console.log(
      `Fresh registry install of ${manifest().name}@${release.version} verified on ${process.platform}/${process.arch} with ${expectedNative}.`,
    );
  } finally {
    rmSync(temporary, { recursive: true, force: true });
  }
}

if (
  process.argv[1] &&
  realpathSync(process.argv[1]) === fileURLToPath(import.meta.url)
) {
  const [mode, bundlePath, sha] = process.argv.slice(2);
  if (!["stage", "published", "install"].includes(mode) || !bundlePath || !sha)
    throw new Error(
      "Usage: verify-registry.mjs <stage|published|install> <bundle-dir> <source-sha>",
    );
  verifyRegistry(mode, bundlePath, sha);
}
