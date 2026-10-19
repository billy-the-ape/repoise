import assert from "node:assert/strict";
import test from "node:test";
import { mkdtempSync, writeFileSync, rmSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { sha1Hex, stageEntries, stagedVersion } from "./verify-registry.mjs";

test("registry helpers match tarball SHAs and normalize stage listings", () => {
  const directory = mkdtempSync(join(tmpdir(), "repoise-registry-"));
  try {
    const file = join(directory, "sample.tgz");
    writeFileSync(file, "hello");
    assert.equal(sha1Hex(file), "aaf4c61ddcc5e8a2dabede0f3b482cd9aea9434d");
    const arrayForm = stageEntries(
      JSON.stringify([{ id: "a", version: "1.0.0" }]),
    );
    assert.deepEqual(stagedVersion(arrayForm, "1.0.0"), [
      { id: "a", version: "1.0.0" },
    ]);
    assert.deepEqual(stagedVersion(arrayForm, "2.0.0"), []);
    const mapForm = stageEntries(
      JSON.stringify({
        "1.0.0": { id: "b", version: "1.0.0" },
        "2.0.0": { id: "c", version: "2.0.0" },
      }),
    );
    assert.deepEqual(stagedVersion(mapForm, "1.0.0"), [
      { id: "b", version: "1.0.0" },
    ]);
    assert.deepEqual(stagedVersion(mapForm, "3.0.0"), []);
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
});
