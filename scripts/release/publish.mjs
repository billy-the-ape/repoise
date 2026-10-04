import { verifyBundle } from "./bundle.mjs";
import { join } from "node:path";
import { root, npm } from "./common.mjs";
const directory = join(root, "dist/bundle");
const release = verifyBundle(directory, process.env.RELEASE_SHA);
for (const name of release.packages) {
  // Stage native packages first, wrapper last. Maintainer approvals must follow that order too.
  npm([
    "stage",
    "publish",
    join(directory, `${name}-${release.version}.tgz`),
    "--access",
    "public",
    "--tag",
    release.tag,
    "--provenance",
    "--ignore-scripts",
    "--registry",
    "https://registry.npmjs.org/",
  ]);
}
