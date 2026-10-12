import { cpSync, mkdirSync, rmSync, writeFileSync, chmodSync } from "node:fs";
import { join, resolve } from "node:path";
import { root, targets, manifest, verifyVersion, npm, run } from "./common.mjs";
const spec = targets.find((item) => item.target === process.argv[2]);
if (!spec) throw new Error("Pass a supported Rust target");
const version = verifyVersion();
const binary = resolve(
  root,
  process.argv[3] ?? `target/${spec.target}/release/${spec.binary}`,
);
if (run(binary, ["--version"]) !== `repoise ${version}`)
  throw new Error("Built binary has wrong version");
const out = join(root, "dist", spec.target);
rmSync(out, { recursive: true, force: true });
mkdirSync(out, { recursive: true });
const directory = join(out, "native");
mkdirSync(join(directory, "bin"), { recursive: true });
cpSync(binary, join(directory, "bin", spec.binary));
chmodSync(join(directory, "bin", spec.binary), 0o755);
cpSync(join(root, "LICENSE"), join(directory, "LICENSE"));
const schemas = join(directory, "schemas");
mkdirSync(schemas, { recursive: true });
cpSync(
  join(root, "schemas/repoise.config.v1.schema.json"),
  join(schemas, "repoise.config.v1.schema.json"),
);
writeFileSync(
  join(directory, "README.md"),
  `# Repoise native binary\n\n${spec.target}; version ${version}. Offline docs+code lexical index, exact reads and stdio MCP; no Node/npm required. MIT licensed.\n`,
);
const config = {
  name: spec.package,
  version,
  description: `Repoise native binary for ${spec.target}`,
  license: "MIT",
  os: [spec.os],
  cpu: [spec.cpu],
  files: ["bin/", "schemas/", "LICENSE", "README.md"],
  repository: { ...manifest().repository, directory: "crates/repoise-cli" },
  publishConfig: manifest().publishConfig,
};
if (spec.libc) config.libc = [spec.libc];
writeFileSync(
  join(directory, "package.json"),
  JSON.stringify(config, null, 2) + "\n",
);
npm([
  "pack",
  directory,
  "--json",
  "--ignore-scripts",
  "--pack-destination",
  out,
]);
const archive = join(out, `repoise-${version}-${spec.target}.tar.gz`);
run("tar", [
  "-czf",
  archive,
  "-C",
  join(directory, "bin"),
  spec.binary,
  "-C",
  directory,
  "LICENSE",
  "README.md",
  "schemas",
]);
console.log(out);
