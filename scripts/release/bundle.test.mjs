import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { mkdtempSync, writeFileSync, rmSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import test from "node:test";
import { expectedFiles, verifyBundle } from "./bundle.mjs";
import { targets } from "./common.mjs";
test("release bundle rejects wrong commit and changed payload", () => {
  const directory = mkdtempSync(join(tmpdir(), "repoise-bundle-"));
  try {
    const version = "0.1.0-alpha.0",
      sha = "a".repeat(40),
      files = expectedFiles(version);
    for (const file of files) writeFileSync(join(directory, file), file);
    writeFileSync(
      join(directory, "release.json"),
      JSON.stringify({
        version,
        sha,
        files,
        packages: [...targets.map((item) => item.package), "repoise"],
        tag: "next",
      }),
    );
    writeFileSync(
      join(directory, "SHA256SUMS"),
      files
        .map(
          (file) =>
            `${createHash("sha256").update(file).digest("hex")}  ${file}\n`,
        )
        .join(""),
    );
    assert.equal(verifyBundle(directory, sha).version, version);
    assert.throws(
      () => verifyBundle(directory, "b".repeat(40)),
      /commit mismatch/,
    );
    writeFileSync(join(directory, files[0]), "corrupted");
    assert.throws(() => verifyBundle(directory, sha), /Checksum mismatch/);
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
});
test("bundle versions cannot become paths", () => {
  assert.throws(() => expectedFiles("../../escape"), /Invalid bundle version/);
});
