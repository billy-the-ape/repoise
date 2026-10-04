import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { targets } from "./common.mjs";
export function expectedFiles(version) {
  if (!/^\d+\.\d+\.\d+(?:-[0-9A-Za-z]+(?:\.[0-9A-Za-z]+)*)?$/.test(version))
    throw new Error("Invalid bundle version");
  return [
    `repoise-${version}.tgz`,
    ...targets.flatMap((item) => [
      `${item.package}-${version}.tgz`,
      `repoise-${version}-${item.target}.tar.gz`,
    ]),
  ].sort();
}
export function verifyBundle(directory, sha) {
  const release = JSON.parse(readFileSync(join(directory, "release.json")));
  if (!/^[0-9a-f]{40}$/.test(sha ?? "") || release.sha !== sha)
    throw new Error("Artifact commit mismatch");
  assert.deepEqual(
    [...release.files].sort(),
    expectedFiles(release.version),
    "Unexpected bundle files",
  );
  assert.deepEqual(
    release.packages,
    [...targets.map((item) => item.package), "repoise"],
    "Unexpected package order",
  );
  assert.equal(release.tag, release.version.includes("-") ? "next" : "latest");
  const sums = readFileSync(join(directory, "SHA256SUMS"), "utf8")
    .trim()
    .split("\n");
  assert.equal(sums.length, release.files.length, "Checksum count mismatch");
  for (const [index, file] of expectedFiles(release.version).entries()) {
    const hash = createHash("sha256")
      .update(readFileSync(join(directory, file)))
      .digest("hex");
    assert.equal(
      sums[index],
      `${hash}  ${file}`,
      `Checksum mismatch for ${file}`,
    );
  }
  return release;
}
