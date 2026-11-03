// Stages the optional benchmark companion distribution and packs it into
// local tarballs (never published). The companion reuses the existing
// `@repoise/agent-adapter-protocol` (host bridge) and
// `@repoise/agent-adapter-cline` (adapter) package implementations with an
// explicit files allowlist, the protocol schemas and license notices.
//
// Usage:
//   node scripts/release/companion.mjs [--out <dir>]
//
// The tarballs land in <dir> (default: dist/bench-companion). The staging
// directory is rebuilt from scratch on every run: only the allowlisted files
// are ever copied, so tests, fixtures, caches and the monorepo never enter a
// companion tarball. No package name is registered or published here — these
// are local artifacts an operator installs with npm (see docs/bench.md).

import {
  cpSync,
  mkdirSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { join, basename } from "node:path";
import { execFileSync } from "node:child_process";
import { root, npm } from "./common.mjs";

const args = process.argv.slice(2);
function argValue(flag) {
  const index = args.indexOf(flag);
  return index >= 0 ? args[index + 1] : undefined;
}
const outDir = argValue("--out") ?? join(root, "dist", "bench-companion");

const version = JSON.parse(
  readFileSync(join(root, "package.json"), "utf8"),
).version;

// The two installable companion packages: the protocol host (the bridge the
// coordinator spawns) and the Cline adapter (drives a separately installed
// Cline CLI). Each lists the exact runtime files it ships.
const packages = [
  {
    flat: "agent-adapter-protocol",
    source: "packages/agent-adapter-protocol",
    name: "@repoise/agent-adapter-protocol",
    description:
      "Repoise agent adapter protocol host (AP-03): the Node bridge `repoise bench` " +
      "spawns to drive an adapter. The optional benchmark companion distribution.",
    files: ["bin", "src", "schemas"],
    schemas: [
      "adapter-request.v1.schema.json",
      "adapter-events.v1.schema.json",
    ],
  },
  {
    flat: "agent-adapter-cline",
    source: "packages/agent-adapter-cline",
    name: "@repoise/agent-adapter-cline",
    description:
      "Repoise Cline agent adapter (AP-03): drives an operator-installed, " +
      "version-pinned Cline CLI under the versioned agent adapter protocol.",
    files: ["bin", "src"],
    schemas: [],
  },
];

function readmeFor(spec) {
  return [
    `# ${spec.name}`,
    "",
    spec.description,
    "",
    "Part of the optional Repoise benchmark companion. Install it into a prefix and",
    "point `REPOISE_BENCH_COMPANION` at this package's root (the directory holding",
    "this package.json). The coordinator validates the package name and version before",
    "executing `bin/adapter-host-run.mjs`. See `docs/bench.md` for the exact",
    "installation and discovery precedence.",
    "",
    `Version: ${version}`,
    "",
  ].join("\n");
}

// Stage each package (allowlisted files only) and pack it to a local tarball.
rmSync(outDir, { recursive: true, force: true });
mkdirSync(outDir, { recursive: true });

const tarballs = [];
for (const spec of packages) {
  const staged = join(outDir, spec.flat);
  cpSync(join(root, spec.source, "bin"), join(staged, "bin"), {
    recursive: true,
  });
  cpSync(join(root, spec.source, "src"), join(staged, "src"), {
    recursive: true,
  });
  if (spec.schemas.length > 0) {
    mkdirSync(join(staged, "schemas"), { recursive: true });
    for (const schema of spec.schemas) {
      writeFileSync(
        join(staged, "schemas", schema),
        readFileSync(join(root, "schemas", schema)),
      );
    }
  }
  writeFileSync(join(staged, "LICENSE"), readFileSync(join(root, "LICENSE")));
  writeFileSync(join(staged, "README.md"), readmeFor(spec));

  // Stage a clean manifest: the source metadata minus the workspace-only
  // fields, plus the explicit files allowlist. `private` and `devDependencies`
  // are dropped so the tarball is installable and ships no development tooling.
  const sourceManifest = JSON.parse(
    readFileSync(join(root, spec.source, "package.json"), "utf8"),
  );
  const {
    private: _private,
    devDependencies: _dev,
    scripts: _scripts,
    ...manifest
  } = sourceManifest;
  writeFileSync(
    join(staged, "package.json"),
    `${JSON.stringify({ ...manifest, files: spec.files }, null, 2)}\n`,
  );

  const packed = npm([
    "pack",
    staged,
    "--json",
    "--ignore-scripts",
    "--pack-destination",
    outDir,
  ]);
  const parsed = JSON.parse(packed);
  const entry = Array.isArray(parsed) ? parsed[0] : parsed;
  const filename = entry.filename ?? entry.id;
  tarballs.push(join(outDir, basename(filename)));
}

// Verify each tarball ships exactly the allowlisted top-level entries (no
// tests, fixtures, transcripts, caches or the monorepo). Listing (tar -tzf)
// is enough and avoids extraction side effects.
for (const tarball of tarballs) {
  const spec = packages.find((candidate) =>
    basename(tarball).includes(candidate.flat),
  );
  if (!spec) throw new Error(`no companion spec for ${tarball}`);
  const listed = execFileSync("tar", ["-tzf", tarball]).toString();
  const entries = listed.split("\n").filter((line) => line.length > 0);
  const allowed = new Set([
    ...spec.files,
    "package.json",
    "README.md",
    "LICENSE",
  ]);
  const topLevel = new Set();
  for (const entry of entries) {
    // npm tarballs nest everything under a top-level `package/` directory.
    if (entry !== "package" && !entry.startsWith("package/")) {
      throw new Error(
        `${basename(tarball)} ships an entry outside package/: ${entry}`,
      );
    }
    const relative = entry === "package" ? "" : entry.slice("package/".length);
    const segment = relative.split("/").find((part) => part.length > 0);
    if (segment) topLevel.add(segment);
  }
  const unexpected = [...topLevel].filter((entry) => !allowed.has(entry));
  if (unexpected.length > 0) {
    throw new Error(
      `${basename(tarball)} ships unexpected entries: ${unexpected.join(", ")}`,
    );
  }
  // Required runtime files must be present.
  for (const required of ["bin", "src", "package.json", "LICENSE"]) {
    if (!topLevel.has(required)) {
      throw new Error(
        `${basename(tarball)} is missing required entry ${required}`,
      );
    }
  }
  if (spec.schemas.length > 0 && !topLevel.has("schemas")) {
    throw new Error(`${basename(tarball)} is missing its schemas directory`);
  }
}

console.log(`Companion tarballs (version ${version}, in ${outDir}):`);
for (const tarball of tarballs) console.log(`  ${basename(tarball)}`);
