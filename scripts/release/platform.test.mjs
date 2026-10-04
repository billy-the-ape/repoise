import assert from "node:assert/strict";
import { createRequire } from "node:module";
import test from "node:test";
const require = createRequire(import.meta.url);
const {
  packageFor,
  binaryPath,
} = require("../../npm/repoise/bin/platform.cjs");
test("supported platforms select exact native package", () => {
  assert.equal(
    packageFor("linux", "x64", { header: { glibcVersionRuntime: "2.39" } }),
    "repoise-linux-x64-gnu",
  );
  assert.equal(packageFor("darwin", "arm64"), "repoise-darwin-arm64");
  assert.equal(packageFor("win32", "x64"), "repoise-win32-x64");
});
test("unsupported architectures and libc fail without fallback", () => {
  for (const [platform, arch, report] of [
    ["linux", "x64", { header: {} }],
    ["darwin", "x64"],
    ["linux", "arm64"],
    ["freebsd", "x64"],
  ])
    assert.throws(
      () => packageFor(platform, arch, report),
      /Unsupported platform/,
    );
});
test("missing optional binary explains recovery", () => {
  assert.throws(
    () =>
      binaryPath("win32", "x64", undefined, () => {
        throw new Error("missing");
      }),
    /optional dependencies enabled/,
  );
});
