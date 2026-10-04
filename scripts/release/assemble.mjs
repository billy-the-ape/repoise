import { createHash } from "node:crypto";
import { readdirSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { root, targets, verifyVersion, manifest, npm } from "./common.mjs";
const version = verifyVersion();
const directory = join(root, "dist/bundle");
npm([
  "pack",
  join(root, "npm/repoise"),
  "--ignore-scripts",
  "--json",
  "--pack-destination",
  directory,
]);
const expected = [
  `repoise-${version}.tgz`,
  ...targets.flatMap((item) => [
    `${item.package}-${version}.tgz`,
    `repoise-${version}-${item.target}.tar.gz`,
  ]),
];
for (const file of expected) {
  if (!readdirSync(directory).includes(file))
    throw new Error(`Missing ${file}`);
}
writeFileSync(
  join(directory, "SHA256SUMS"),
  expected
    .sort()
    .map(
      (file) =>
        `${createHash("sha256")
          .update(readFileSync(join(directory, file)))
          .digest("hex")}  ${file}\n`,
    )
    .join(""),
);
writeFileSync(
  join(directory, "release.json"),
  JSON.stringify(
    {
      version,
      sha: process.env.RELEASE_SHA,
      packages: [...targets.map((item) => item.package), manifest().name],
      tag: version.includes("-") ? "next" : "latest",
      files: expected,
    },
    null,
    2,
  ) + "\n",
);
