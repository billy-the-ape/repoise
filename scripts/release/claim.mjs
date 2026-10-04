import { cpSync, mkdirSync, rmSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { root, manifest, npm } from "./common.mjs";
const directory = join(root, "dist/claim/package");
rmSync(join(root, "dist/claim"), { recursive: true, force: true });
mkdirSync(directory, { recursive: true });
cpSync(join(root, "npm/repoise"), directory, { recursive: true });
const config = manifest();
config.version = "0.0.0-bootstrap.0";
delete config.optionalDependencies;
config.description =
  "Repoise name-claim staging package; native CLI release pending";
writeFileSync(
  join(directory, "package.json"),
  JSON.stringify(config, null, 2) + "\n",
);
writeFileSync(
  join(directory, "README.md"),
  "# Repoise staging bootstrap\n\nThis package establishes npm ownership for Repoise. It has no native binary and is not a usable release. Do not approve this staged bootstrap. See https://github.com/billy-the-ape/repoise for development. MIT licensed.\n",
);
const packed = JSON.parse(
  npm([
    "pack",
    directory,
    "--json",
    "--ignore-scripts",
    "--pack-destination",
    join(root, "dist/claim"),
  ]),
)[0];
console.log(
  `Prepared dist/claim/${packed.filename}. No registry write was performed.`,
);
